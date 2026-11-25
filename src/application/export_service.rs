use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::application::validation_service::ValidationService;
use crate::domain::asset::Asset;
use crate::domain::ports::{
    ExportAssetCopy, ExportPlan, ExportReportRow, ExportSummary, PackageExporter,
    ProjectRepository, XmlEngine,
};
use crate::domain::project::{ProjectMode, ProjectSnapshot};
use crate::domain::record::RecordValue;
use crate::domain::validation::ValidationSeverity;
use crate::domain::variable::{Variable, VariableKind};
use crate::domain::xml_model::{
    ParsedXmlTemplate, PrefixUpdateReport, TemplateVariableTrait, XmlCellValue, XmlColumnBinding,
    XmlDatasetEntry, XmlExportSpec,
};
use crate::infrastructure::export::AtomicPackageExporter;
use crate::infrastructure::xml::{IllustratorXmlEngine, format_asset_target};
use crate::shared::error::{AppError, AppResult, DomainError};

pub struct ExportService {
    repo: Arc<dyn ProjectRepository>,
    xml_engine: IllustratorXmlEngine,
    exporter: AtomicPackageExporter,
    validation: ValidationService,
}

impl ExportService {
    pub fn new(repo: Arc<dyn ProjectRepository>) -> Self {
        let validation = ValidationService::new(Arc::clone(&repo));
        Self {
            repo,
            xml_engine: IllustratorXmlEngine::new(),
            exporter: AtomicPackageExporter::new(),
            validation,
        }
    }

    pub fn inspect_xml_template(&self, xml_raw: &str) -> AppResult<ParsedXmlTemplate> {
        self.xml_engine.inspect_template(xml_raw)
    }

    /// Validates `xml_raw` and binds it to the project (preserving existing records and columns).
    pub fn bind_xml_template(
        &self,
        project_id: &str,
        template_path: Option<&Path>,
        xml_raw: &str,
    ) -> AppResult<ParsedXmlTemplate> {
        let clean_raw = xml_raw.strip_prefix('\u{FEFF}').unwrap_or(xml_raw);
        let parsed = self.xml_engine.inspect_template(clean_raw)?;

        let mut snap = self.repo.open_project(project_id)?;
        snap.project.mode = ProjectMode::WithTemplate;
        snap.project.xml_template_path = template_path.map(Path::to_path_buf);
        snap.project.xml_template_raw = Some(clean_raw.to_string());
        self.repo.update_project_settings(&snap.project)?;

        Ok(parsed)
    }

    /// Generates an in-memory XML preview using native absolute paths inside `preview_dir`
    /// (falling back to `last_export_dir` or current working directory).
    pub fn generate_xml_preview(&self, project_id: &str) -> AppResult<String> {
        self.generate_xml_preview_for_dir(project_id, None)
    }

    pub fn generate_xml_preview_for_dir(
        &self,
        project_id: &str,
        preview_dir: Option<&Path>,
    ) -> AppResult<String> {
        let snap = self.repo.open_project(project_id)?;
        let preview_base = preview_dir
            .map(Path::to_path_buf)
            .or_else(|| snap.project.last_export_dir.clone())
            .unwrap_or_else(|| PathBuf::from("."));
        let (spec, _) = self.build_xml_spec_and_copies(&snap, &preview_base)?;
        self.xml_engine.generate_xml(&spec)
    }

    pub fn validate_xml_structure(&self, xml_raw: &str, expected_datasets: usize) -> AppResult<()> {
        self.xml_engine
            .validate_xml_structure(xml_raw, expected_datasets)
    }

    /// Audits the project, generates `dados.xml`, copies only linked photos (`asset_001.ext`, ...),
    /// and atomically publishes `dados.xml`, `imagens/`, `manifest.json`, and `relatorio.csv`.
    pub fn export_package(
        &self,
        project_id: &str,
        target_dir: &Path,
        overwrite_confirmed: bool,
    ) -> AppResult<ExportSummary> {
        let audit = self.validation.audit_project(project_id)?;
        if !audit.can_export {
            return Err(AppError::Domain(DomainError::PreExportBlocked {
                blocking_count: audit.blocking_count,
            }));
        }

        let mut snap = self.repo.open_project(project_id)?;
        let (spec, asset_copies) = self.build_xml_spec_and_copies(&snap, target_dir)?;
        let xml_content = self.xml_engine.generate_xml(&spec)?;

        // Map per-record warning messages and asset filenames for `relatorio.csv`.
        let mut warnings_by_record: HashMap<&str, Vec<String>> = HashMap::new();
        for issue in &audit.issues {
            if issue.severity == ValidationSeverity::Warning {
                match &issue.target {
                    crate::domain::validation::ValidationTarget::Record { record_id }
                    | crate::domain::validation::ValidationTarget::Cell { record_id, .. } => {
                        warnings_by_record
                            .entry(record_id.as_str())
                            .or_default()
                            .push(format!("[{}] {}", issue.code.code_str(), issue.message));
                    }
                    _ => {}
                }
            }
        }

        let rel_by_asset_id: HashMap<&str, &str> = asset_copies
            .iter()
            .map(|c| (c.asset_id.as_str(), c.target_rel_path.as_str()))
            .collect();
        let val_map: HashMap<(&str, &str), &RecordValue> = snap
            .values
            .iter()
            .map(|v| ((v.record_id.as_str(), v.variable_id.as_str()), v))
            .collect();
        let linked_var_ids: Vec<&str> = snap
            .variables
            .iter()
            .filter(|v| v.kind == VariableKind::LinkedFile)
            .map(|v| v.id.as_str())
            .collect();

        let report_rows: Vec<ExportReportRow> = snap
            .records
            .iter()
            .map(|rec| {
                let mut mapped_assets = Vec::new();
                for &vid in &linked_var_ids {
                    if let Some(aid) = val_map
                        .get(&(rec.id.as_str(), vid))
                        .and_then(|v| v.asset_id.as_deref())
                        && let Some(&rel) = rel_by_asset_id.get(aid)
                    {
                        mapped_assets.push(rel.to_string());
                    }
                }
                let rec_warnings = warnings_by_record
                    .get(rec.id.as_str())
                    .cloned()
                    .unwrap_or_default();
                let status = if rec_warnings.is_empty() {
                    "OK".to_string()
                } else {
                    "AVISO".to_string()
                };
                ExportReportRow {
                    record_id: rec.id.clone(),
                    dataset_name: rec.dataset_name.clone(),
                    status,
                    mapped_assets,
                    warnings: rec_warnings,
                }
            })
            .collect();

        let plan = ExportPlan {
            project_id: snap.project.id.clone(),
            project_name: snap.project.name.clone(),
            target_dir: target_dir.to_path_buf(),
            path_strategy: snap.project.path_strategy,
            xml_content,
            expected_datasets: snap.records.len(),
            asset_copies,
            report_rows,
            audit_warnings_count: audit.warning_count,
        };

        let summary = self.exporter.export_atomic(&plan, overwrite_confirmed)?;

        snap.project.last_export_dir = Some(target_dir.to_path_buf());
        self.repo.update_project_settings(&snap.project)?;

        Ok(summary)
    }

    pub fn update_exported_prefix(&self, export_dir: &Path) -> AppResult<PrefixUpdateReport> {
        self.xml_engine.update_export_prefix(export_dir)
    }

    fn build_xml_spec_and_copies(
        &self,
        snap: &ProjectSnapshot,
        target_dir: &Path,
    ) -> AppResult<(XmlExportSpec, Vec<ExportAssetCopy>)> {
        let asset_map: HashMap<&str, &Asset> =
            snap.assets.iter().map(|a| (a.id.as_str(), a)).collect();
        let val_map: HashMap<(&str, &str), &RecordValue> = snap
            .values
            .iter()
            .map(|v| ((v.record_id.as_str(), v.variable_id.as_str()), v))
            .collect();

        // Order exported columns to match template <variables> order when WithTemplate is active.
        let ordered_cols: Vec<(&Variable, String)> =
            if snap.project.mode == ProjectMode::WithTemplate {
                if let Some(ref tpl_raw) = snap.project.xml_template_raw {
                    let parsed = self.xml_engine.inspect_template(tpl_raw)?;
                    let mut by_target: HashMap<&str, &Variable> = HashMap::new();
                    for v in &snap.variables {
                        let key = v
                            .mapped_template_var
                            .as_deref()
                            .unwrap_or(v.xml_name.as_str());
                        by_target.insert(key, v);
                    }
                    let mut out = Vec::new();
                    let mut included_var_ids: HashSet<&str> = HashSet::new();
                    for tv in &parsed.variables {
                        if matches!(
                            tv.trait_kind,
                            TemplateVariableTrait::TextContent | TemplateVariableTrait::Fileref
                        ) && let Some(&v) = by_target.get(tv.var_name.as_str())
                        {
                            included_var_ids.insert(v.id.as_str());
                            out.push((v, tv.var_name.clone()));
                        }
                    }
                    for v in &snap.variables {
                        if !included_var_ids.contains(v.id.as_str()) {
                            let tag = v
                                .mapped_template_var
                                .clone()
                                .unwrap_or_else(|| v.xml_name.clone());
                            out.push((v, tag));
                        }
                    }
                    out
                } else {
                    snap.variables
                        .iter()
                        .map(|v| (v, v.xml_name.clone()))
                        .collect()
                }
            } else {
                snap.variables
                    .iter()
                    .map(|v| (v, v.xml_name.clone()))
                    .collect()
            };

        let mut asset_rel_by_id: HashMap<String, String> = HashMap::new();
        let mut asset_copies: Vec<ExportAssetCopy> = Vec::new();

        for rec in &snap.records {
            for (var, _) in &ordered_cols {
                if var.kind == VariableKind::LinkedFile
                    && let Some(aid) = val_map
                        .get(&(rec.id.as_str(), var.id.as_str()))
                        .and_then(|v| v.asset_id.as_deref())
                    && !asset_rel_by_id.contains_key(aid)
                    && let Some(asset) = asset_map.get(aid)
                {
                    let idx = asset_copies.len() + 1;
                    let rel_path = format!("imagens/asset_{idx:03}.{}", asset.format.canonical_ext());
                    asset_rel_by_id.insert(aid.to_string(), rel_path.clone());
                    asset_copies.push(ExportAssetCopy {
                        asset_id: asset.id.clone(),
                        source_path: asset.source_path.clone(),
                        target_rel_path: rel_path,
                        format: asset.format,
                    });
                }
            }
        }

        let columns: Vec<XmlColumnBinding> = ordered_cols
            .iter()
            .map(|(v, tag_name)| XmlColumnBinding {
                xml_tag_name: tag_name.clone(),
                kind: v.kind,
            })
            .collect();

        let datasets: Vec<XmlDatasetEntry> = snap
            .records
            .iter()
            .map(|rec| {
                let values = ordered_cols
                    .iter()
                    .map(|(var, _)| match var.kind {
                        VariableKind::Text => {
                            let txt = val_map
                                .get(&(rec.id.as_str(), var.id.as_str()))
                                .map(|v| v.text_value.clone())
                                .unwrap_or_default();
                            XmlCellValue::Text(txt)
                        }
                        VariableKind::LinkedFile => {
                            let rel_opt = val_map
                                .get(&(rec.id.as_str(), var.id.as_str()))
                                .and_then(|v| v.asset_id.as_deref())
                                .and_then(|aid| asset_rel_by_id.get(aid));
                            let rendered = match rel_opt {
                                Some(rel) => format_asset_target(target_dir, rel),
                                None => String::new(),
                            };
                            XmlCellValue::LinkedFile(rendered)
                        }
                    })
                    .collect();
                XmlDatasetEntry {
                    dataset_name: rec.dataset_name.clone(),
                    values,
                }
            })
            .collect();

        let var_set_name = snap
            .project
            .xml_template_raw
            .as_deref()
            .and_then(|raw| self.xml_engine.inspect_template(raw).ok())
            .and_then(|p| p.var_set_name)
            .unwrap_or_else(|| "ConjuntoVariaveis".to_string());

        Ok((
            XmlExportSpec {
                mode: snap.project.mode,
                path_strategy: snap.project.path_strategy,
                template_raw: snap.project.xml_template_raw.clone(),
                var_set_name,
                columns,
                datasets,
            },
            asset_copies,
        ))
    }
}
