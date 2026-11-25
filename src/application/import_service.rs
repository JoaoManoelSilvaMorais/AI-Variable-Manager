use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use uuid::Uuid;

use crate::application::data_service::next_unique_dataset_name;
use crate::domain::ports::{
    ImportColumnMapping, ImportColumnTarget, ImportMode, ProjectRepository, RawTable,
    SpreadsheetReader,
};
use crate::domain::record::{Record, RecordValue};
use crate::domain::variable::{
    Variable, ensure_unique_xml_name, validate_display_name, validate_xml_name,
};
use crate::infrastructure::spreadsheet::CompositeSpreadsheetReader;
use crate::shared::error::{AppError, AppResult};
use crate::shared::text_norm::{resolve_unique_xml_name, suggest_xml_name};
use crate::shared::time::utc_now_rfc3339;

pub struct ImportService {
    repo: Arc<dyn ProjectRepository>,
    reader: CompositeSpreadsheetReader,
}

impl ImportService {
    pub fn new(repo: Arc<dyn ProjectRepository>) -> Self {
        Self {
            repo,
            reader: CompositeSpreadsheetReader::new(),
        }
    }

    pub fn list_sheets(&self, path: &Path) -> AppResult<Vec<String>> {
        self.reader.list_sheets(path)
    }

    pub fn preview_spreadsheet(
        &self,
        path: &Path,
        sheet: Option<&str>,
        max_rows: Option<usize>,
    ) -> AppResult<RawTable> {
        self.reader.read_table(path, sheet, max_rows)
    }

    /// Commits a mapped `.xlsx` or `.csv` import into the project, preserving text formatting
    /// (including leading zeros) without modifying the source spreadsheet file.
    pub fn commit_import(
        &self,
        project_id: &str,
        path: &Path,
        sheet: Option<&str>,
        header_row: usize,
        mappings: &[ImportColumnMapping],
        mode: ImportMode,
    ) -> AppResult<usize> {
        let snap = self.repo.open_project(project_id)?;
        let table = self.reader.read_table(path, sheet, None)?;

        if table.rows.is_empty() {
            return Err(AppError::validation(
                "A planilha selecionada está vazia e não possui linha de cabeçalho.",
            ));
        }
        if header_row >= table.rows.len() {
            return Err(AppError::validation(format!(
                "Linha de cabeçalho ({}) fora do intervalo da planilha (total de {} linha(s)).",
                header_row + 1,
                table.rows.len()
            )));
        }

        let active_mappings: Vec<&ImportColumnMapping> = mappings
            .iter()
            .filter(|m| !matches!(m.target, ImportColumnTarget::Ignore))
            .collect();
        if active_mappings.is_empty() {
            return Ok(0);
        }

        // Reject duplicate ExistingVariable targets and validate all existing references up front.
        let mut seen_existing_targets: HashSet<&str> = HashSet::new();
        for mapping in &active_mappings {
            if let ImportColumnTarget::ExistingVariable(var_id) = &mapping.target {
                if snap.variable_by_id(var_id).is_none() {
                    return Err(AppError::not_found("Variável", var_id));
                }
                if !seen_existing_targets.insert(var_id.as_str()) {
                    return Err(AppError::validation(
                        "Duas colunas de origem não podem ser mapeadas para a mesma variável.",
                    ));
                }
            }
        }

        // Resolve or create variables cleanly before inserting records.
        let mut simulated_vars = snap.variables.clone();
        let mut next_var_order = simulated_vars
            .iter()
            .map(|v| v.sort_order)
            .max()
            .map(|m| m + 1)
            .unwrap_or(0);
        let mut vars_to_create = Vec::new();
        let mut resolved_col_vars: Vec<(usize, String)> = Vec::with_capacity(active_mappings.len());

        for mapping in &active_mappings {
            match &mapping.target {
                ImportColumnTarget::ExistingVariable(var_id) => {
                    resolved_col_vars.push((mapping.source_col_index, var_id.clone()));
                }
                ImportColumnTarget::NewVariable(spec) => {
                    let display_name = validate_display_name(&spec.display_name)?;
                    let xml_name = match &spec.xml_name {
                        Some(explicit) => {
                            let trimmed = explicit.trim();
                            validate_xml_name(trimmed)?;
                            ensure_unique_xml_name(trimmed, &simulated_vars, None)?;
                            trimmed.to_string()
                        }
                        None => {
                            let base = suggest_xml_name(&display_name);
                            resolve_unique_xml_name(
                                &base,
                                simulated_vars.iter().map(|v| v.xml_name.as_str()),
                            )
                        }
                    };
                    let new_var = Variable {
                        id: Uuid::new_v4().to_string(),
                        project_id: project_id.to_string(),
                        display_name,
                        xml_name,
                        kind: spec.kind,
                        required: spec.required,
                        sort_order: next_var_order,
                        mapped_template_var: spec.mapped_template_var.clone(),
                    };
                    next_var_order += 1;
                    simulated_vars.push(new_var.clone());
                    resolved_col_vars.push((mapping.source_col_index, new_var.id.clone()));
                    vars_to_create.push(new_var);
                }
                ImportColumnTarget::Ignore => {}
            }
        }

        for var in &vars_to_create {
            self.repo.upsert_variable(var)?;
        }

        if mode == ImportMode::Replace && !snap.records.is_empty() {
            let existing_ids: Vec<String> = snap.records.iter().map(|r| r.id.clone()).collect();
            self.repo.delete_records(project_id, &existing_ids)?;
        }

        let data_rows = &table.rows[header_row + 1..];
        if data_rows.is_empty() {
            return Ok(0);
        }

        let mut used_ds_names: HashSet<String> = if mode == ImportMode::Replace {
            HashSet::new()
        } else {
            snap.records
                .iter()
                .map(|r| r.dataset_name.clone())
                .collect()
        };

        let mut next_rec_order = if mode == ImportMode::Replace {
            0
        } else {
            snap.records
                .iter()
                .map(|r| r.sort_order)
                .max()
                .map(|m| m + 1)
                .unwrap_or(0)
        };

        let now = utc_now_rfc3339();
        let mut records_to_insert = Vec::with_capacity(data_rows.len());
        let mut values_to_insert = Vec::with_capacity(data_rows.len() * resolved_col_vars.len());

        for row in data_rows {
            let ds_name = next_unique_dataset_name(next_rec_order + 1, &used_ds_names);
            used_ds_names.insert(ds_name.clone());

            let rec_id = Uuid::new_v4().to_string();
            records_to_insert.push(Record {
                id: rec_id.clone(),
                project_id: project_id.to_string(),
                dataset_name: ds_name,
                sort_order: next_rec_order,
                created_at: now.clone(),
                updated_at: now.clone(),
            });
            next_rec_order += 1;

            for (src_col_idx, var_id) in &resolved_col_vars {
                let cell_text = row.get(*src_col_idx).cloned().unwrap_or_default();
                values_to_insert.push(RecordValue::new_text(&rec_id, var_id, cell_text));
            }
        }

        self.repo
            .upsert_records_and_values(project_id, &records_to_insert, &values_to_insert)?;
        Ok(records_to_insert.len())
    }
}
