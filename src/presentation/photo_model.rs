use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;

use slint::{Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};

use crate::application::AppServices;
use crate::domain::asset::{Asset, MatchCandidate, PhotoDiagnostics};
use crate::domain::ports::{ThumbnailData, ThumbnailState};
use crate::domain::project::ProjectSnapshot;
use crate::domain::record::{MatchStatus, RecordValue};
use crate::domain::variable::{Variable, VariableKind};
use crate::presentation::{PhotoAssetItem, PhotoCandidateItem, PhotoCellItem};
use crate::shared::error::AppResult;

#[derive(Debug, Default)]
pub struct PhotoViewState {
    pub selected_photo_col_index: usize,
    pub filter_index: usize, // 0=All, 1=Confirmed, 2=Suggested/Ambiguous, 3=Missing/Unmatched
    pub selected_cell_index: Option<usize>,
    pub selected_pool_asset_index: Option<usize>,
    pub pool_filter_text: String,
    pub batch_selected_record_ids: HashSet<String>,
    pub loaded_thumbs: HashSet<(String, String)>, // (asset_id, path_str) already requested or ready
}

pub fn thumbnail_to_slint_image(data: &ThumbnailData) -> Image {
    let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        &data.rgba8,
        data.width,
        data.height,
    );
    Image::from_rgba8(buffer)
}

pub struct PhotoTabBundle {
    pub diagnostics: PhotoDiagnostics,
    pub photo_col_names: ModelRc<SharedString>,
    pub selected_var_id: Option<String>,
    pub cells: Vec<PhotoCellItem>,
    pub candidates: Vec<PhotoCandidateItem>,
    pub unmatched_assets: Vec<PhotoAssetItem>,
    pub has_loading_thumbs: bool,
}

pub fn build_photo_tab_bundle(
    services: &AppServices,
    snap: &ProjectSnapshot,
    state: &mut PhotoViewState,
    auto_request_visible: bool,
) -> AppResult<PhotoTabBundle> {
    let diagnostics = services
        .photo_service
        .compute_diagnostics(&snap.project.id, false)?;

    let photo_vars: Vec<&Variable> = snap
        .variables
        .iter()
        .filter(|v| v.kind == VariableKind::LinkedFile)
        .collect();
    let photo_col_names = ModelRc::new(Rc::new(VecModel::from(
        photo_vars
            .iter()
            .map(|v| SharedString::from(v.display_name.as_str()))
            .collect::<Vec<_>>(),
    )));

    if photo_vars.is_empty() {
        return Ok(PhotoTabBundle {
            diagnostics,
            photo_col_names,
            selected_var_id: None,
            cells: Vec::new(),
            candidates: Vec::new(),
            unmatched_assets: Vec::new(),
            has_loading_thumbs: false,
        });
    }

    if state.selected_photo_col_index >= photo_vars.len() {
        state.selected_photo_col_index = 0;
    }
    let active_var = photo_vars[state.selected_photo_col_index];

    let asset_map: HashMap<&str, &Asset> = snap
        .assets
        .iter()
        .map(|a| (a.id.as_str(), a))
        .collect();
    let val_map: HashMap<(&str, &str), &RecordValue> = snap
        .values
        .iter()
        .map(|v| ((v.record_id.as_str(), v.variable_id.as_str()), v))
        .collect();

    let mut cand_counts: HashMap<(&str, &str), usize> = HashMap::new();
    for c in &snap.match_candidates {
        *cand_counts
            .entry((c.record_id.as_str(), c.variable_id.as_str()))
            .or_insert(0) += 1;
    }

    let mut has_loading_thumbs = false;
    let mut cells = Vec::new();

    for rec in &snap.records {
        let rv = val_map.get(&(rec.id.as_str(), active_var.id.as_str())).copied();
        let search_text = rv.map(|v| v.text_value.as_str()).unwrap_or("");
        let asset_id = rv.and_then(|v| v.asset_id.as_deref());
        let asset = asset_id.and_then(|aid| asset_map.get(aid).copied());

        let (status_kind, status_label) = match asset {
            Some(a) if !a.exists_on_disk => (4, "Ausente no disco"),
            Some(_) => {
                let st = rv.map(|v| v.match_status).unwrap_or(MatchStatus::Manual);
                if st == MatchStatus::Manual {
                    (3, "Manual")
                } else if st.is_confirmed_link() {
                    (3, "Match Auto")
                } else {
                    (3, "Vinculada")
                }
            }
            None => match rv.map(|v| v.match_status).unwrap_or(MatchStatus::Unmatched) {
                MatchStatus::AutoSuggested => (1, "Sugestão"),
                MatchStatus::Ambiguous => (2, "Ambígua"),
                _ => (0, "Sem foto"),
            },
        };

        let include = match state.filter_index {
            1 => status_kind == 3,
            2 => status_kind == 1 || status_kind == 2,
            3 => status_kind == 0 || status_kind == 4,
            _ => true,
        };
        if !include {
            continue;
        }

        let (linked_file_name, linked_path, has_thumb, thumb) = if let Some(a) = asset {
            let path_str = a.source_path.to_string_lossy().into_owned();
            let (ready, loading, img) =
                resolve_thumb_if_loaded(services, &a.id, &a.source_path, auto_request_visible && cells.len() < 24);
            has_loading_thumbs |= loading;
            (a.file_name.as_str(), path_str, ready, img)
        } else {
            ("", String::new(), false, Image::default())
        };

        let record_columns_text = build_record_columns_summary(
            &snap.variables,
            &active_var.id,
            &rec.id,
            &val_map,
            &asset_map,
        );

        cells.push(PhotoCellItem {
            record_id: rec.id.as_str().into(),
            variable_id: active_var.id.as_str().into(),
            dataset_name: rec.dataset_name.as_str().into(),
            record_columns_text: record_columns_text.into(),
            variable_name: active_var.display_name.as_str().into(),
            search_text: search_text.into(),
            status_kind,
            status_label: status_label.into(),
            linked_file_name: linked_file_name.into(),
            linked_path: linked_path.into(),
            has_thumb,
            thumb,
            candidate_count: cand_counts
                .get(&(rec.id.as_str(), active_var.id.as_str()))
                .copied()
                .unwrap_or(0) as i32,
            selected_for_batch: state.batch_selected_record_ids.contains(&rec.id),
        });
    }

    if state
        .selected_cell_index
        .is_some_and(|idx| idx >= cells.len())
    {
        state.selected_cell_index = None;
    }

    let mut candidates = Vec::new();
    if let Some(sel_idx) = state.selected_cell_index
        && let Some(cell) = cells.get(sel_idx)
    {
        let mut cell_cands: Vec<&MatchCandidate> = snap
            .match_candidates
            .iter()
            .filter(|c| {
                c.record_id == cell.record_id.as_str()
                    && c.variable_id == cell.variable_id.as_str()
            })
            .collect();
        cell_cands.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for cand in cell_cands {
            if let Some(a) = asset_map.get(cand.asset_id.as_str()) {
                let (has_thumb, loading, thumb) =
                    resolve_thumb_if_loaded(services, &a.id, &a.source_path, true);
                has_loading_thumbs |= loading;
                let rel_path = relative_display_path(snap.project.photo_root_dir.as_deref(), &a.source_path);
                candidates.push(PhotoCandidateItem {
                    asset_id: a.id.as_str().into(),
                    file_name: a.file_name.as_str().into(),
                    relative_path: rel_path.into(),
                    full_path: a.source_path.to_string_lossy().as_ref().into(),
                    confidence_label: format!("Confiança {:.0}% ({})", cand.score * 100.0, cand.reason).into(),
                    has_thumb,
                    thumb,
                });
            }
        }
    }

    let pool = services
        .photo_service
        .get_unmatched_pool(&snap.project.id, &active_var.id)?;
    let filter_lower = state.pool_filter_text.trim().to_ascii_lowercase();
    let mut unmatched_assets = Vec::new();
    for a in &pool.assets {
        if !filter_lower.is_empty()
            && !a.file_name.to_ascii_lowercase().contains(&filter_lower)
        {
            continue;
        }
        let (has_thumb, loading, thumb) = resolve_thumb_if_loaded(
            services,
            &a.id,
            &a.source_path,
            auto_request_visible && unmatched_assets.len() < 16,
        );
        has_loading_thumbs |= loading;
        let rel_path = relative_display_path(snap.project.photo_root_dir.as_deref(), &a.source_path);
        unmatched_assets.push(PhotoAssetItem {
            asset_id: a.id.as_str().into(),
            file_name: a.file_name.as_str().into(),
            relative_path: rel_path.into(),
            full_path: a.source_path.to_string_lossy().as_ref().into(),
            format_label: a.format.canonical_ext().to_ascii_uppercase().into(),
            size_label: format_bytes(a.file_size_bytes).into(),
            is_linked: false,
            exists_on_disk: a.exists_on_disk,
            has_thumb,
            thumb,
        });
    }

    if state
        .selected_pool_asset_index
        .is_some_and(|idx| idx >= unmatched_assets.len())
    {
        state.selected_pool_asset_index = None;
    }

    Ok(PhotoTabBundle {
        diagnostics,
        photo_col_names,
        selected_var_id: Some(active_var.id.clone()),
        cells,
        candidates,
        unmatched_assets,
        has_loading_thumbs,
    })
}

pub fn resolve_thumb_if_loaded(
    services: &AppServices,
    asset_id: &str,
    path: &Path,
    request_if_missing: bool,
) -> (bool, bool, Image) {
    if !request_if_missing {
        return (false, false, Image::default());
    }
    match services.photo_service.get_or_request_thumbnail(asset_id, path) {
        Ok(ThumbnailState::Ready(data)) => (true, false, thumbnail_to_slint_image(&data)),
        Ok(ThumbnailState::Loading) => (false, true, Image::default()),
        Ok(ThumbnailState::Failed(_)) | Err(_) => (false, false, Image::default()),
    }
}

fn build_record_columns_summary(
    variables: &[Variable],
    active_var_id: &str,
    record_id: &str,
    val_map: &HashMap<(&str, &str), &RecordValue>,
    asset_map: &HashMap<&str, &Asset>,
) -> String {
    let mut parts = Vec::new();
    for var in variables {
        if var.id == active_var_id {
            continue;
        }
        let rv = val_map.get(&(record_id, var.id.as_str())).copied();
        let display_val = match var.kind {
            VariableKind::Text => {
                let raw = rv.map(|v| v.text_value.as_str()).unwrap_or("");
                let single_line = raw
                    .replace("\r\n", " / ")
                    .replace(['\n', '\r'], " / ");
                let trimmed = single_line.trim();
                if trimmed.is_empty() {
                    "—".to_string()
                } else {
                    trimmed.to_string()
                }
            }
            VariableKind::LinkedFile => {
                if let Some(aid) = rv.and_then(|v| v.asset_id.as_deref())
                    && let Some(asset) = asset_map.get(aid)
                {
                    asset.file_name.clone()
                } else {
                    let raw = rv.map(|v| v.text_value.trim()).unwrap_or("");
                    if raw.is_empty() {
                        "(sem foto)".to_string()
                    } else {
                        raw.to_string()
                    }
                }
            }
        };
        parts.push(format!("{}: {}", var.display_name, display_val));
    }
    parts.join("  •  ")
}

fn relative_display_path(root: Option<&Path>, path: &Path) -> String {
    root.and_then(|r| path.strip_prefix(r).ok())
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}
