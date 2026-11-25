use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use uuid::Uuid;

use crate::domain::asset::{
    Asset, BatchLinkResult, DuplicateAssetAssignment, MatchCandidate, PhotoDiagnostics,
    ScanResult, UnmatchedPool,
};
use crate::domain::ports::{
    PhotoScanner, ProjectRepository, ThumbnailCache, ThumbnailState,
};
use crate::domain::record::{MatchStatus, Record, RecordValue};
use crate::domain::variable::VariableKind;
use crate::infrastructure::media::{
    LocalPhotoScanner, LruThumbnailCache, inspect_single_image_file, run_auto_match_for_snapshot,
};
use crate::shared::error::{AppError, AppResult};

pub struct PhotoService {
    repo: Arc<dyn ProjectRepository>,
    scanner: LocalPhotoScanner,
    thumb_cache: LruThumbnailCache,
}

impl PhotoService {
    pub fn new(repo: Arc<dyn ProjectRepository>) -> Self {
        Self {
            repo,
            scanner: LocalPhotoScanner::new(),
            thumb_cache: LruThumbnailCache::default(),
        }
    }

    /// Scans `root_dir` for supported photos (metadata only) and updates the project's asset catalog,
    /// preserving IDs for unchanged `source_path`s.
    pub fn scan_directory(&self, project_id: &str, root_dir: &Path) -> AppResult<ScanResult> {
        let mut snap = self.repo.open_project(project_id)?;
        let scan = self.scanner.scan_directory(root_dir)?;

        let existing_by_path: HashMap<&Path, &Asset> = snap
            .assets
            .iter()
            .map(|a| (a.source_path.as_path(), a))
            .collect();

        let new_assets: Vec<Asset> = scan
            .assets
            .iter()
            .cloned()
            .map(|scanned| {
                let id = existing_by_path
                    .get(scanned.source_path.as_path())
                    .map(|existing| existing.id.clone())
                    .unwrap_or_else(|| Uuid::new_v4().to_string());
                scanned.into_asset(id, project_id.to_string())
            })
            .collect();

        self.repo.replace_assets(project_id, &new_assets)?;
        snap.project.photo_root_dir = Some(root_dir.to_path_buf());
        self.repo.update_project_settings(&snap.project)?;

        Ok(scan)
    }

    /// Runs Method A auto-matching across all `LinkedFile` columns and returns updated diagnostics.
    pub fn run_auto_match(&self, project_id: &str) -> AppResult<PhotoDiagnostics> {
        let snap = self.repo.open_project(project_id)?;
        let output = run_auto_match_for_snapshot(&snap);

        // Keep any existing candidates belonging to preserved Manual cells (none needed since Manual clears them)
        if !output.updated_values.is_empty() {
            self.repo
                .upsert_records_and_values(project_id, &[], &output.updated_values)?;
        }
        self.repo
            .update_match_candidates(project_id, &output.candidates)?;

        self.compute_diagnostics(project_id, false)
    }

    /// Accepts an `AutoSuggested` or `Ambiguous` candidate, promoting it to a confirmed link.
    pub fn accept_match_candidate(
        &self,
        project_id: &str,
        record_id: &str,
        variable_id: &str,
        asset_id: &str,
    ) -> AppResult<()> {
        let snap = self.repo.open_project(project_id)?;
        if !snap.records.iter().any(|r| r.id == record_id) {
            return Err(AppError::not_found("Registro", record_id));
        }
        let var = snap
            .variable_by_id(variable_id)
            .ok_or_else(|| AppError::not_found("Variável", variable_id))?;
        if var.kind != VariableKind::LinkedFile {
            return Err(AppError::validation(
                "Apenas colunas do tipo 'Fotografia (Vínculo)' podem receber fotos.",
            ));
        }
        let asset = snap
            .asset_by_id(asset_id)
            .ok_or_else(|| AppError::not_found("Ativo", asset_id))?;
        if !asset.exists_on_disk || !asset.source_path.exists() {
            return Err(AppError::validation(
                "O arquivo de fotografia selecionado não existe no disco.",
            ));
        }

        let prev_text = snap
            .value_for(record_id, variable_id)
            .map(|v| v.text_value.clone())
            .unwrap_or_default();

        let new_val = RecordValue {
            record_id: record_id.to_string(),
            variable_id: variable_id.to_string(),
            text_value: prev_text,
            asset_id: Some(asset_id.to_string()),
            match_status: MatchStatus::Manual,
            match_confidence: Some(1.0),
        };

        let remaining_candidates: Vec<MatchCandidate> = snap
            .match_candidates
            .into_iter()
            .filter(|c| !(c.record_id == record_id && c.variable_id == variable_id))
            .collect();

        self.repo
            .upsert_records_and_values(project_id, &[], &[new_val])?;
        self.repo
            .update_match_candidates(project_id, &remaining_candidates)
    }

    /// Method B: manually links `image_path` to `(record_id, variable_id)`, registering external
    /// valid images in project assets if not already scanned.
    pub fn link_manual(
        &self,
        project_id: &str,
        record_id: &str,
        variable_id: &str,
        image_path: &Path,
    ) -> AppResult<()> {
        let snap = self.repo.open_project(project_id)?;
        if !snap.records.iter().any(|r| r.id == record_id) {
            return Err(AppError::not_found("Registro", record_id));
        }
        let var = snap
            .variable_by_id(variable_id)
            .ok_or_else(|| AppError::not_found("Variável", variable_id))?;
        if var.kind != VariableKind::LinkedFile {
            return Err(AppError::validation(
                "Não é possível vincular fotografia em uma coluna de texto.",
            ));
        }

        let scanned = inspect_single_image_file(image_path)?;

        let asset_id = if let Some(existing) = snap
            .assets
            .iter()
            .find(|a| a.source_path == image_path)
        {
            let mut updated_assets = snap.assets.clone();
            if let Some(slot) = updated_assets.iter_mut().find(|a| a.id == existing.id) {
                slot.exists_on_disk = true;
                slot.file_size_bytes = scanned.file_size_bytes;
                slot.modified_epoch_ms = scanned.modified_epoch_ms;
            }
            self.repo.replace_assets(project_id, &updated_assets)?;
            existing.id.clone()
        } else {
            let new_id = Uuid::new_v4().to_string();
            let new_asset = scanned.into_asset(new_id.clone(), project_id.to_string());
            let mut updated_assets = snap.assets.clone();
            updated_assets.push(new_asset);
            self.repo.replace_assets(project_id, &updated_assets)?;
            new_id
        };

        let prev_text = snap
            .value_for(record_id, variable_id)
            .map(|v| v.text_value.clone())
            .unwrap_or_default();

        let new_val = RecordValue {
            record_id: record_id.to_string(),
            variable_id: variable_id.to_string(),
            text_value: prev_text,
            asset_id: Some(asset_id),
            match_status: MatchStatus::Manual,
            match_confidence: Some(1.0),
        };

        let remaining_candidates: Vec<MatchCandidate> = snap
            .match_candidates
            .into_iter()
            .filter(|c| !(c.record_id == record_id && c.variable_id == variable_id))
            .collect();

        self.repo
            .upsert_records_and_values(project_id, &[], &[new_val])?;
        self.repo
            .update_match_candidates(project_id, &remaining_candidates)
    }

    /// Removes the photo association from `(record_id, variable_id)`, restoring `Unmatched` status.
    pub fn unlink_photo(
        &self,
        project_id: &str,
        record_id: &str,
        variable_id: &str,
    ) -> AppResult<()> {
        let snap = self.repo.open_project(project_id)?;
        if !snap.records.iter().any(|r| r.id == record_id) {
            return Err(AppError::not_found("Registro", record_id));
        }
        if snap.variable_by_id(variable_id).is_none() {
            return Err(AppError::not_found("Variável", variable_id));
        }

        let prev_text = snap
            .value_for(record_id, variable_id)
            .map(|v| v.text_value.clone())
            .unwrap_or_default();

        let cleared = RecordValue::new_text(record_id, variable_id, prev_text);
        let remaining_candidates: Vec<MatchCandidate> = snap
            .match_candidates
            .into_iter()
            .filter(|c| !(c.record_id == record_id && c.variable_id == variable_id))
            .collect();

        self.repo
            .upsert_records_and_values(project_id, &[], &[cleared])?;
        self.repo
            .update_match_candidates(project_id, &remaining_candidates)
    }

    /// Method C: returns unmatched records for `variable_id` and scanned assets not currently linked.
    pub fn get_unmatched_pool(
        &self,
        project_id: &str,
        variable_id: &str,
    ) -> AppResult<UnmatchedPool> {
        let snap = self.repo.open_project(project_id)?;
        let var = snap
            .variable_by_id(variable_id)
            .ok_or_else(|| AppError::not_found("Variável", variable_id))?;
        if var.kind != VariableKind::LinkedFile {
            return Err(AppError::validation(
                "O pool de fotos sem correspondência exige uma coluna de fotografia.",
            ));
        }

        let val_map: HashMap<(&str, &str), &RecordValue> = snap
            .values
            .iter()
            .map(|v| ((v.record_id.as_str(), v.variable_id.as_str()), v))
            .collect();

        let used_asset_ids: HashSet<&str> = snap
            .values
            .iter()
            .filter_map(|v| v.asset_id.as_deref())
            .collect();

        let records: Vec<Record> = snap
            .records
            .iter()
            .filter(|r| {
                val_map
                    .get(&(r.id.as_str(), variable_id))
                    .and_then(|v| v.asset_id.as_ref())
                    .is_none()
            })
            .cloned()
            .collect();

        let assets: Vec<Asset> = snap
            .assets
            .iter()
            .filter(|a| !used_asset_ids.contains(a.id.as_str()))
            .cloned()
            .collect();

        Ok(UnmatchedPool { records, assets })
    }

    /// Method C: batch-pairs `(record_id, variable_id, asset_id)` tuples atomically.
    pub fn batch_link(
        &self,
        project_id: &str,
        pairs: &[(String, String, String)],
        overwrite_existing: bool,
    ) -> AppResult<BatchLinkResult> {
        let snap = self.repo.open_project(project_id)?;
        if pairs.is_empty() {
            return Ok(BatchLinkResult {
                linked_count: 0,
                duplicate_asset_warnings: Vec::new(),
            });
        }

        let record_set: HashSet<&str> = snap.records.iter().map(|r| r.id.as_str()).collect();
        let var_map: HashMap<&str, VariableKind> = snap
            .variables
            .iter()
            .map(|v| (v.id.as_str(), v.kind))
            .collect();
        let asset_map: HashMap<&str, &Asset> =
            snap.assets.iter().map(|a| (a.id.as_str(), a)).collect();

        let mut working_values: HashMap<(String, String), RecordValue> = snap
            .values
            .iter()
            .map(|v| ((v.record_id.clone(), v.variable_id.clone()), v.clone()))
            .collect();

        let mut updated_values = Vec::with_capacity(pairs.len());
        let mut cleared_cells: HashSet<(String, String)> = HashSet::with_capacity(pairs.len());

        for (rec_id, var_id, asset_id) in pairs {
            if !record_set.contains(rec_id.as_str()) {
                return Err(AppError::not_found("Registro", rec_id));
            }
            match var_map.get(var_id.as_str()) {
                None => return Err(AppError::not_found("Variável", var_id)),
                Some(kind) if *kind != VariableKind::LinkedFile => {
                    return Err(AppError::validation(
                        "Vínculo em lote permitido apenas em colunas de fotografia.",
                    ));
                }
                Some(_) => {}
            }
            let asset = asset_map
                .get(asset_id.as_str())
                .ok_or_else(|| AppError::not_found("Ativo", asset_id))?;
            if !asset.exists_on_disk || !asset.source_path.exists() {
                return Err(AppError::validation(format!(
                    "A fotografia '{}' não existe mais no disco.",
                    asset.file_name
                )));
            }

            let cell_key = (rec_id.clone(), var_id.clone());
            let prev = working_values.get(&cell_key);
            if !overwrite_existing && prev.and_then(|v| v.asset_id.as_ref()).is_some() {
                return Err(AppError::conflict(
                    "O registro selecionado já possui fotografia vinculada.",
                ));
            }

            let text_value = prev.map(|v| v.text_value.clone()).unwrap_or_default();
            let new_val = RecordValue {
                record_id: rec_id.clone(),
                variable_id: var_id.clone(),
                text_value,
                asset_id: Some(asset_id.clone()),
                match_status: MatchStatus::Manual,
                match_confidence: Some(1.0),
            };
            working_values.insert(cell_key.clone(), new_val.clone());
            cleared_cells.insert(cell_key);
            updated_values.push(new_val);
        }

        let remaining_candidates: Vec<MatchCandidate> = snap
            .match_candidates
            .into_iter()
            .filter(|c| !cleared_cells.contains(&(c.record_id.clone(), c.variable_id.clone())))
            .collect();

        self.repo
            .upsert_records_and_values(project_id, &[], &updated_values)?;
        self.repo
            .update_match_candidates(project_id, &remaining_candidates)?;

        let duplicate_asset_warnings = compute_duplicate_assignments(&working_values);

        Ok(BatchLinkResult {
            linked_count: updated_values.len(),
            duplicate_asset_warnings,
        })
    }

    /// Computes photo match diagnostics, optionally refreshing physical disk existence of assets.
    pub fn compute_diagnostics(
        &self,
        project_id: &str,
        refresh_disk_existence: bool,
    ) -> AppResult<PhotoDiagnostics> {
        let mut snap = self.repo.open_project(project_id)?;

        if refresh_disk_existence && !snap.assets.is_empty() {
            let mut changed = false;
            for asset in &mut snap.assets {
                let exists_now = asset.source_path.is_file();
                if asset.exists_on_disk != exists_now {
                    asset.exists_on_disk = exists_now;
                    changed = true;
                }
            }
            if changed {
                self.repo.replace_assets(project_id, &snap.assets)?;
            }
        }

        let linked_var_ids: Vec<&str> = snap
            .variables
            .iter()
            .filter(|v| v.kind == VariableKind::LinkedFile)
            .map(|v| v.id.as_str())
            .collect();

        let val_map: HashMap<(&str, &str), &RecordValue> = snap
            .values
            .iter()
            .map(|v| ((v.record_id.as_str(), v.variable_id.as_str()), v))
            .collect();

        let mut matched_count = 0usize;
        let mut suggested_count = 0usize;
        let mut ambiguous_count = 0usize;
        let mut missing_records_set: HashSet<String> = HashSet::new();
        let mut linked_asset_ids: HashSet<&str> = HashSet::new();
        let mut asset_to_records: BTreeMap<String, Vec<String>> = BTreeMap::new();

        for &var_id in &linked_var_ids {
            for rec in &snap.records {
                let rv = val_map.get(&(rec.id.as_str(), var_id)).copied();
                match rv.and_then(|v| v.asset_id.as_deref()) {
                    Some(aid) => {
                        linked_asset_ids.insert(aid);
                        let list = asset_to_records.entry(aid.to_string()).or_default();
                        if !list.contains(&rec.id) {
                            list.push(rec.id.clone());
                        }
                        if rv.is_some_and(|v| v.match_status.is_confirmed_link()) {
                            matched_count += 1;
                        }
                    }
                    None => {
                        missing_records_set.insert(rec.id.clone());
                        if let Some(v) = rv {
                            match v.match_status {
                                MatchStatus::AutoSuggested => suggested_count += 1,
                                MatchStatus::Ambiguous => ambiguous_count += 1,
                                _ => {}
                            }
                        }
                    }
                }
            }
        }

        let records_missing_photo: Vec<String> = snap
            .records
            .iter()
            .filter(|r| missing_records_set.contains(&r.id))
            .map(|r| r.id.clone())
            .collect();

        let unmatched_asset_ids: Vec<String> = snap
            .assets
            .iter()
            .filter(|a| !linked_asset_ids.contains(a.id.as_str()))
            .map(|a| a.id.clone())
            .collect();

        let duplicate_asset_assignments: Vec<DuplicateAssetAssignment> = asset_to_records
            .into_iter()
            .filter(|(_, recs)| recs.len() > 1)
            .map(|(asset_id, record_ids)| DuplicateAssetAssignment {
                asset_id,
                record_ids,
            })
            .collect();

        let missing_on_disk_asset_ids: Vec<String> = snap
            .assets
            .iter()
            .filter(|a| !a.exists_on_disk)
            .map(|a| a.id.clone())
            .collect();

        Ok(PhotoDiagnostics {
            total_records: snap.records.len(),
            total_scanned_assets: snap.assets.len(),
            matched_count,
            suggested_count,
            ambiguous_count,
            records_missing_photo,
            unmatched_asset_ids,
            duplicate_asset_assignments,
            missing_on_disk_asset_ids,
        })
    }

    pub fn get_or_request_thumbnail(&self, asset_id: &str, path: &Path) -> AppResult<ThumbnailState> {
        Ok(self.thumb_cache.get_or_request(asset_id, path))
    }

    pub fn clear_thumbnail_cache(&self) -> AppResult<()> {
        self.thumb_cache.clear();
        Ok(())
    }

    pub fn thumbnail_cache_len(&self) -> AppResult<usize> {
        Ok(self.thumb_cache.len())
    }
}

fn compute_duplicate_assignments(
    values: &HashMap<(String, String), RecordValue>,
) -> Vec<DuplicateAssetAssignment> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for val in values.values() {
        if let Some(ref aid) = val.asset_id {
            let list = map.entry(aid.clone()).or_default();
            if !list.contains(&val.record_id) {
                list.push(val.record_id.clone());
            }
        }
    }
    map.into_iter()
        .filter(|(_, recs)| recs.len() > 1)
        .map(|(asset_id, record_ids)| DuplicateAssetAssignment {
            asset_id,
            record_ids,
        })
        .collect()
}
