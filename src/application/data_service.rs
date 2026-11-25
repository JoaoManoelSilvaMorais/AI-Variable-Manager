use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use uuid::Uuid;

use crate::domain::ports::ProjectRepository;
use crate::domain::record::{
    CellDiagnostic, CellDiagnosticKind, GridWindow, MatchStatus, Record, RecordValue,
};
use crate::domain::variable::VariableKind;
use crate::infrastructure::spreadsheet::parse_clipboard_block;
use crate::shared::error::{AppError, AppResult};
use crate::shared::time::utc_now_rfc3339;

pub struct DataService {
    repo: Arc<dyn ProjectRepository>,
}

impl DataService {
    pub fn new(repo: Arc<dyn ProjectRepository>) -> Self {
        Self { repo }
    }

    /// Adds a new empty record at the end of the project's grid with a unique `dataset_name`.
    pub fn add_record(&self, project_id: &str, dataset_name: Option<&str>) -> AppResult<Record> {
        let snap = self.repo.open_project(project_id)?;
        let now = utc_now_rfc3339();
        let next_order = snap
            .records
            .iter()
            .map(|r| r.sort_order)
            .max()
            .map(|m| m + 1)
            .unwrap_or(0);

        let used_names: HashSet<String> = snap
            .records
            .iter()
            .map(|r| r.dataset_name.clone())
            .collect();

        let ds_name = match dataset_name {
            Some(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    return Err(AppError::validation(
                        "O nome do conjunto de dados (Data Set) não pode estar vazio.",
                    ));
                }
                trimmed.to_string()
            }
            None => next_unique_dataset_name(next_order + 1, &used_names),
        };

        let record = Record {
            id: Uuid::new_v4().to_string(),
            project_id: project_id.to_string(),
            dataset_name: ds_name,
            sort_order: next_order,
            created_at: now.clone(),
            updated_at: now,
        };

        self.repo
            .upsert_records_and_values(project_id, std::slice::from_ref(&record), &[])?;
        Ok(record)
    }

    /// Updates a single cell's literal `text_value` (preserving spaces and leading zeros).
    pub fn update_cell(
        &self,
        project_id: &str,
        record_id: &str,
        variable_id: &str,
        value: &str,
    ) -> AppResult<()> {
        let snap = self.repo.open_project(project_id)?;
        if !snap.records.iter().any(|r| r.id == record_id) {
            return Err(AppError::not_found("Registro", record_id));
        }
        let var = snap
            .variable_by_id(variable_id)
            .ok_or_else(|| AppError::not_found("Variável", variable_id))?;

        let existing = snap.value_for(record_id, variable_id);
        let new_val = match (var.kind, existing) {
            (VariableKind::LinkedFile, Some(prev)) if prev.text_value == value => RecordValue {
                record_id: record_id.to_string(),
                variable_id: variable_id.to_string(),
                text_value: value.to_string(),
                asset_id: prev.asset_id.clone(),
                match_status: prev.match_status,
                match_confidence: prev.match_confidence,
            },
            _ => RecordValue::new_text(record_id, variable_id, value),
        };

        self.repo
            .upsert_records_and_values(project_id, &[], &[new_val])
    }

    /// Updates the `dataset_name` of an existing record.
    pub fn update_dataset_name(
        &self,
        project_id: &str,
        record_id: &str,
        dataset_name: &str,
    ) -> AppResult<()> {
        let trimmed = dataset_name.trim();
        if trimmed.is_empty() {
            return Err(AppError::validation(
                "O nome do conjunto de dados (Data Set) não pode estar vazio.",
            ));
        }
        let snap = self.repo.open_project(project_id)?;
        let mut rec = snap
            .records
            .into_iter()
            .find(|r| r.id == record_id)
            .ok_or_else(|| AppError::not_found("Registro", record_id))?;
        rec.dataset_name = trimmed.to_string();
        rec.updated_at = utc_now_rfc3339();
        self.repo.upsert_records_and_values(project_id, &[rec], &[])
    }

    /// Deletes one or more records and reindexes remaining rows contiguously.
    pub fn delete_records(&self, project_id: &str, record_ids: &[String]) -> AppResult<()> {
        self.repo.delete_records(project_id, record_ids)
    }

    /// Persists a new ordering of all records in the project.
    pub fn reorder_records(&self, project_id: &str, ordered_ids: &[String]) -> AppResult<()> {
        self.repo.reorder_records(project_id, ordered_ids)
    }

    /// Returns a contiguous slice (`offset..offset+limit`) of records and their values.
    pub fn get_records_window(
        &self,
        project_id: &str,
        offset: usize,
        limit: usize,
    ) -> AppResult<GridWindow> {
        let snap = self.repo.open_project(project_id)?;
        let total_records = snap.records.len();
        if offset >= total_records || limit == 0 {
            return Ok(GridWindow {
                total_records,
                offset,
                records: Vec::new(),
                values: Vec::new(),
            });
        }

        let end = offset.saturating_add(limit).min(total_records);
        let sliced_records: Vec<Record> = snap.records[offset..end].to_vec();
        let rec_ids: HashSet<&str> = sliced_records.iter().map(|r| r.id.as_str()).collect();
        let sliced_values: Vec<RecordValue> = snap
            .values
            .into_iter()
            .filter(|v| rec_ids.contains(v.record_id.as_str()))
            .collect();

        Ok(GridWindow {
            total_records,
            offset,
            records: sliced_records,
            values: sliced_values,
        })
    }

    /// Pastes a TSV/CSV clipboard block starting at `(start_row, start_col)`, expanding rows
    /// automatically when needed and clamping extra columns beyond project column count.
    pub fn paste_block(
        &self,
        project_id: &str,
        start_row: usize,
        start_col: usize,
        clipboard_text: &str,
    ) -> AppResult<usize> {
        let parsed_rows = parse_clipboard_block(clipboard_text)?;
        if parsed_rows.is_empty() {
            return Ok(0);
        }

        let snap = self.repo.open_project(project_id)?;
        if snap.variables.is_empty() {
            return Err(AppError::validation(
                "Crie ao menos uma coluna antes de colar blocos de dados.",
            ));
        }
        if start_col >= snap.variables.len() {
            return Err(AppError::validation(
                "Coluna inicial fora dos limites da tabela.",
            ));
        }

        let existing_vals: HashMap<(&str, &str), &RecordValue> = snap
            .values
            .iter()
            .map(|v| ((v.record_id.as_str(), v.variable_id.as_str()), v))
            .collect();
        let mut used_ds_names: HashSet<String> = snap
            .records
            .iter()
            .map(|r| r.dataset_name.clone())
            .collect();
        let mut next_sort_order = snap
            .records
            .iter()
            .map(|r| r.sort_order)
            .max()
            .map(|m| m + 1)
            .unwrap_or(0);

        let now = utc_now_rfc3339();
        let mut records_to_upsert = Vec::with_capacity(parsed_rows.len());
        let mut values_to_upsert = Vec::new();

        for (row_offset, cells) in parsed_rows.iter().enumerate() {
            let target_row = start_row + row_offset;
            let rec = if target_row < snap.records.len() {
                let mut existing_rec = snap.records[target_row].clone();
                existing_rec.updated_at = now.clone();
                existing_rec
            } else {
                let ds_name = next_unique_dataset_name(next_sort_order + 1, &used_ds_names);
                used_ds_names.insert(ds_name.clone());
                let rec = Record {
                    id: Uuid::new_v4().to_string(),
                    project_id: project_id.to_string(),
                    dataset_name: ds_name,
                    sort_order: next_sort_order,
                    created_at: now.clone(),
                    updated_at: now.clone(),
                };
                next_sort_order += 1;
                rec
            };

            for (col_offset, cell_text) in cells.iter().enumerate() {
                let target_col = start_col + col_offset;
                if target_col >= snap.variables.len() {
                    break;
                }
                let var = &snap.variables[target_col];
                let prev = existing_vals.get(&(rec.id.as_str(), var.id.as_str()));
                let val = match (var.kind, prev) {
                    (VariableKind::LinkedFile, Some(p)) if p.text_value == *cell_text => {
                        (*p).clone()
                    }
                    _ => RecordValue::new_text(&rec.id, &var.id, cell_text),
                };
                values_to_upsert.push(val);
            }

            records_to_upsert.push(rec);
        }

        self.repo
            .upsert_records_and_values(project_id, &records_to_upsert, &values_to_upsert)?;
        Ok(parsed_rows.len())
    }

    /// Computes per-cell diagnostics (empty cells, exact duplicates, and photo link states).
    pub fn compute_cell_diagnostics(&self, project_id: &str) -> AppResult<Vec<CellDiagnostic>> {
        let snap = self.repo.open_project(project_id)?;
        let val_map: HashMap<(&str, &str), &RecordValue> = snap
            .values
            .iter()
            .map(|v| ((v.record_id.as_str(), v.variable_id.as_str()), v))
            .collect();
        let asset_map: HashMap<&str, bool> = snap
            .assets
            .iter()
            .map(|a| (a.id.as_str(), a.exists_on_disk))
            .collect();

        let mut diagnostics = Vec::new();

        for var in &snap.variables {
            match var.kind {
                VariableKind::Text => {
                    let mut value_counts: HashMap<&str, usize> = HashMap::new();
                    for rec in &snap.records {
                        let text = val_map
                            .get(&(rec.id.as_str(), var.id.as_str()))
                            .map(|v| v.text_value.as_str())
                            .unwrap_or("");
                        if !text.trim().is_empty() {
                            *value_counts.entry(text).or_insert(0) += 1;
                        }
                    }

                    for rec in &snap.records {
                        let text = val_map
                            .get(&(rec.id.as_str(), var.id.as_str()))
                            .map(|v| v.text_value.as_str())
                            .unwrap_or("");
                        if text.trim().is_empty() {
                            let (kind, message) = if var.required {
                                (
                                    CellDiagnosticKind::EmptyRequired,
                                    format!("Campo obrigatório '{}' está vazio.", var.display_name),
                                )
                            } else {
                                (
                                    CellDiagnosticKind::EmptyOptional,
                                    format!("Campo opcional '{}' está vazio.", var.display_name),
                                )
                            };
                            diagnostics.push(CellDiagnostic {
                                record_id: rec.id.clone(),
                                variable_id: var.id.clone(),
                                kind,
                                message,
                            });
                        } else if value_counts.get(text).copied().unwrap_or(0) > 1 {
                            diagnostics.push(CellDiagnostic {
                                record_id: rec.id.clone(),
                                variable_id: var.id.clone(),
                                kind: CellDiagnosticKind::DuplicateValue,
                                message: format!(
                                    "Valor duplicado em '{}': '{}'.",
                                    var.display_name, text
                                ),
                            });
                        }
                    }
                }
                VariableKind::LinkedFile => {
                    for rec in &snap.records {
                        let rv = val_map.get(&(rec.id.as_str(), var.id.as_str())).copied();
                        let text = rv.map(|v| v.text_value.as_str()).unwrap_or("");
                        let asset_id = rv.and_then(|v| v.asset_id.as_deref());
                        let status = rv.map(|v| v.match_status).unwrap_or(MatchStatus::Unmatched);

                        if let Some(aid) = asset_id {
                            if !asset_map.get(aid).copied().unwrap_or(false) {
                                diagnostics.push(CellDiagnostic {
                                    record_id: rec.id.clone(),
                                    variable_id: var.id.clone(),
                                    kind: CellDiagnosticKind::MissingAssetOnDisk,
                                    message: format!(
                                        "Arquivo vinculado em '{}' não existe no disco.",
                                        var.display_name
                                    ),
                                });
                            }
                        } else if status == MatchStatus::Ambiguous {
                            diagnostics.push(CellDiagnostic {
                                record_id: rec.id.clone(),
                                variable_id: var.id.clone(),
                                kind: CellDiagnosticKind::AmbiguousPhotoMatch,
                                message: format!(
                                    "Múltiplas fotografias candidatas para '{}'.",
                                    var.display_name
                                ),
                            });
                        } else if status == MatchStatus::AutoSuggested {
                            diagnostics.push(CellDiagnostic {
                                record_id: rec.id.clone(),
                                variable_id: var.id.clone(),
                                kind: CellDiagnosticKind::SuggestedPhotoPending,
                                message: format!(
                                    "Sugestão de fotografia aguardando confirmação em '{}'.",
                                    var.display_name
                                ),
                            });
                        } else if text.trim().is_empty() {
                            let (kind, message) = if var.required {
                                (
                                    CellDiagnosticKind::EmptyRequired,
                                    format!("Fotografia obrigatória '{}' não informada.", var.display_name),
                                )
                            } else {
                                (
                                    CellDiagnosticKind::EmptyOptional,
                                    format!("Fotografia opcional '{}' sem vínculo.", var.display_name),
                                )
                            };
                            diagnostics.push(CellDiagnostic {
                                record_id: rec.id.clone(),
                                variable_id: var.id.clone(),
                                kind,
                                message,
                            });
                        } else {
                            diagnostics.push(CellDiagnostic {
                                record_id: rec.id.clone(),
                                variable_id: var.id.clone(),
                                kind: CellDiagnosticKind::MissingAssetLink,
                                message: format!(
                                    "Chave '{}' em '{}' ainda não vinculada a uma foto.",
                                    text, var.display_name
                                ),
                            });
                        }
                    }
                }
            }
        }

        Ok(diagnostics)
    }
}

pub(crate) fn next_unique_dataset_name(start_index: usize, used: &HashSet<String>) -> String {
    let mut counter = start_index.max(1);
    loop {
        let candidate = format!("Registro_{counter:04}");
        if !used.contains(&candidate) {
            return candidate;
        }
        counter += 1;
    }
}
