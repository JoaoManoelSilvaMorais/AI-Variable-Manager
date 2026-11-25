use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use slint::{ModelRc, SharedString, VecModel};

use crate::application::AppServices;
use crate::domain::ports::{ImportColumnMapping, ImportColumnTarget, RawTable};
use crate::domain::NewVariableSpec;
use crate::domain::project::ProjectSnapshot;
use crate::domain::record::{CellDiagnostic, CellDiagnosticKind, MatchStatus, RecordValue};
use crate::domain::variable::{Variable, VariableKind};
use crate::presentation::{GridCellItem, GridRowItem, ImportColumnItem, ImportPreviewRowItem, VariableItem};
use crate::shared::error::AppResult;

pub const GRID_PAGE_SIZE: usize = 50;

#[derive(Debug, Clone)]
pub struct ImportColumnDraft {
    pub source_index: usize,
    pub source_header: String,
    pub sample_preview: String,
    pub target_mode: i32, // 0 = Ignore, 1 = CreateText, 2 = CreatePhoto, 3 = ExistingVar
    pub new_name: String,
    pub existing_var_index: i32,
}

#[derive(Debug, Default)]
pub struct ImportWizardState {
    pub open: bool,
    pub file_path: Option<PathBuf>,
    pub sheets: Vec<String>,
    pub selected_sheet_index: usize,
    pub header_row_1based: usize,
    pub columns: Vec<ImportColumnDraft>,
    pub raw_table: Option<RawTable>,
}

pub struct GridViewState {
    pub current_page: usize,
    pub selected_record_ids: HashSet<String>,
    pub import: ImportWizardState,
}

impl Default for GridViewState {
    fn default() -> Self {
        Self {
            current_page: 1,
            selected_record_ids: HashSet::new(),
            import: ImportWizardState {
                header_row_1based: 1,
                ..Default::default()
            },
        }
    }
}

pub fn build_variable_items(variables: &[Variable]) -> ModelRc<VariableItem> {
    let items: Vec<VariableItem> = variables
        .iter()
        .map(|v| VariableItem {
            id: v.id.as_str().into(),
            display_name: v.display_name.as_str().into(),
            xml_name: v.xml_name.as_str().into(),
            kind_label: v.kind.display_label_pt_br().into(),
            is_photo: v.kind == VariableKind::LinkedFile,
            required: v.required,
            mapped_template_var: v
                .mapped_template_var
                .as_deref()
                .unwrap_or("")
                .into(),
        })
        .collect();
    ModelRc::new(Rc::new(VecModel::from(items)))
}

pub fn build_existing_var_names(variables: &[Variable]) -> ModelRc<SharedString> {
    let names: Vec<SharedString> = variables
        .iter()
        .map(|v| SharedString::from(v.display_name.as_str()))
        .collect();
    ModelRc::new(Rc::new(VecModel::from(names)))
}

pub fn build_grid_page(
    services: &AppServices,
    snap: &ProjectSnapshot,
    state: &mut GridViewState,
) -> AppResult<(ModelRc<GridRowItem>, usize, usize)> {
    let total_records = snap.records.len();
    let total_pages = total_records.div_ceil(GRID_PAGE_SIZE).max(1);
    if state.current_page < 1 {
        state.current_page = 1;
    }
    if state.current_page > total_pages {
        state.current_page = total_pages;
    }

    let valid_ids: HashSet<&str> = snap.records.iter().map(|r| r.id.as_str()).collect();
    state.selected_record_ids.retain(|id| valid_ids.contains(id.as_str()));

    let offset = (state.current_page - 1) * GRID_PAGE_SIZE;
    let window = services
        .data_service
        .get_records_window(&snap.project.id, offset, GRID_PAGE_SIZE)?;

    let diags = services
        .data_service
        .compute_cell_diagnostics(&snap.project.id)?;
    let mut diag_map: HashMap<(&str, &str), Vec<&CellDiagnostic>> = HashMap::new();
    for d in &diags {
        diag_map
            .entry((d.record_id.as_str(), d.variable_id.as_str()))
            .or_default()
            .push(d);
    }

    let val_map: HashMap<(&str, &str), &RecordValue> = window
        .values
        .iter()
        .map(|v| ((v.record_id.as_str(), v.variable_id.as_str()), v))
        .collect();

    let rows: Vec<GridRowItem> = window
        .records
        .iter()
        .enumerate()
        .map(|(local_idx, rec)| {
            let mut row_has_issue = false;
            let cells: Vec<GridCellItem> = snap
                .variables
                .iter()
                .map(|var| {
                    let rv = val_map.get(&(rec.id.as_str(), var.id.as_str())).copied();
                    let text = rv.map(|v| v.text_value.as_str()).unwrap_or("");
                    let cell_diags = diag_map
                        .get(&(rec.id.as_str(), var.id.as_str()))
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);

                    let is_duplicate = cell_diags
                        .iter()
                        .any(|d| d.kind == CellDiagnosticKind::DuplicateValue);
                    let is_missing_error = cell_diags.iter().any(|d| {
                        matches!(
                            d.kind,
                            CellDiagnosticKind::EmptyRequired
                                | CellDiagnosticKind::MissingAssetOnDisk
                                | CellDiagnosticKind::MissingAssetLink
                                | CellDiagnosticKind::AmbiguousPhotoMatch
                        )
                    });
                    if is_missing_error {
                        row_has_issue = true;
                    }

                    let (status_kind, status_label) = match var.kind {
                        VariableKind::Text => {
                            if is_missing_error {
                                (3, "Obrig.")
                            } else if is_duplicate {
                                (2, "Dup.")
                            } else if cell_diags
                                .iter()
                                .any(|d| d.kind == CellDiagnosticKind::EmptyOptional)
                            {
                                (1, "Vazio")
                            } else {
                                (0, "")
                            }
                        }
                        VariableKind::LinkedFile => {
                            let asset_missing = cell_diags
                                .iter()
                                .any(|d| d.kind == CellDiagnosticKind::MissingAssetOnDisk);
                            if asset_missing {
                                (3, "Ausente")
                            } else {
                                let status = rv.map(|v| v.match_status).unwrap_or(MatchStatus::Unmatched);
                                if status.is_confirmed_link() {
                                    (5, "OK")
                                } else {
                                    match status {
                                        MatchStatus::AutoSuggested => (4, "Sug."),
                                        MatchStatus::Ambiguous => (3, "Amb."),
                                        MatchStatus::Unmatched => {
                                            if var.required {
                                                (3, "Sem foto")
                                            } else {
                                                (1, "Sem foto")
                                            }
                                        }
                                        _ => (5, "OK"),
                                    }
                                }
                            }
                        }
                    };

                    GridCellItem {
                        variable_id: var.id.as_str().into(),
                        text: text.into(),
                        is_photo: var.kind == VariableKind::LinkedFile,
                        status_kind,
                        status_label: status_label.into(),
                        is_duplicate,
                        is_missing_error,
                    }
                })
                .collect();

            GridRowItem {
                record_id: rec.id.as_str().into(),
                row_number: (offset + local_idx + 1) as i32,
                dataset_name: rec.dataset_name.as_str().into(),
                selected: state.selected_record_ids.contains(&rec.id),
                has_issue: row_has_issue,
                cells: ModelRc::new(Rc::new(VecModel::from(cells))),
            }
        })
        .collect();

    Ok((ModelRc::new(Rc::new(VecModel::from(rows))), total_records, total_pages))
}

pub fn rebuild_import_columns(
    table: &RawTable,
    header_row_1based: usize,
    existing_vars: &[Variable],
) -> (Vec<ImportColumnDraft>, Vec<ImportPreviewRowItem>) {
    let header_idx = header_row_1based.saturating_sub(1);
    let headers_row = table.rows.get(header_idx).cloned().unwrap_or_default();
    let sample_row = table.rows.get(header_idx + 1).cloned().unwrap_or_default();
    let col_count = headers_row
        .len()
        .max(table.rows.iter().map(Vec::len).max().unwrap_or(0));

    let mut columns = Vec::with_capacity(col_count);
    for idx in 0..col_count {
        let raw_hdr = headers_row
            .get(idx)
            .map(String::as_str)
            .unwrap_or("")
            .trim();
        let header_name = if raw_hdr.is_empty() {
            format!("Coluna {}", idx + 1)
        } else {
            raw_hdr.to_string()
        };
        let sample = sample_row
            .get(idx)
            .map(String::as_str)
            .unwrap_or("")
            .trim()
            .to_string();

        let existing_idx = existing_vars.iter().position(|v| {
            v.display_name.eq_ignore_ascii_case(&header_name)
                || v.xml_name.eq_ignore_ascii_case(&header_name)
        });
        let looks_like_photo = {
            let lower = header_name.to_ascii_lowercase();
            lower.contains("foto") || lower.contains("imagem") || lower.contains("photo")
        };

        let (target_mode, existing_var_index) = if let Some(found) = existing_idx {
            (3, found as i32)
        } else if looks_like_photo {
            (2, 0)
        } else {
            (1, 0)
        };

        columns.push(ImportColumnDraft {
            source_index: idx,
            source_header: header_name.clone(),
            sample_preview: sample,
            target_mode,
            new_name: header_name,
            existing_var_index,
        });
    }

    let preview_rows: Vec<ImportPreviewRowItem> = table
        .rows
        .iter()
        .take(6)
        .enumerate()
        .map(|(ridx, row)| ImportPreviewRowItem {
            row_index: (ridx + 1) as i32,
            cells: ModelRc::new(Rc::new(VecModel::from(
                row.iter()
                    .map(|c| SharedString::from(c.as_str()))
                    .collect::<Vec<_>>(),
            ))),
        })
        .collect();

    (columns, preview_rows)
}

pub fn build_import_column_models(cols: &[ImportColumnDraft]) -> ModelRc<ImportColumnItem> {
    let items: Vec<ImportColumnItem> = cols
        .iter()
        .map(|c| ImportColumnItem {
            source_index: c.source_index as i32,
            source_header: c.source_header.as_str().into(),
            sample_preview: c.sample_preview.as_str().into(),
            target_mode: c.target_mode,
            new_name: c.new_name.as_str().into(),
            existing_var_index: c.existing_var_index,
        })
        .collect();
    ModelRc::new(Rc::new(VecModel::from(items)))
}

pub fn compile_import_mappings(
    cols: &[ImportColumnDraft],
    existing_vars: &[Variable],
) -> Vec<ImportColumnMapping> {
    cols.iter()
        .map(|col| {
            let target = match col.target_mode {
                1 => ImportColumnTarget::NewVariable(NewVariableSpec {
                    display_name: if col.new_name.trim().is_empty() {
                        col.source_header.clone()
                    } else {
                        col.new_name.clone()
                    },
                    xml_name: None,
                    kind: VariableKind::Text,
                    required: false,
                    mapped_template_var: None,
                }),
                2 => ImportColumnTarget::NewVariable(NewVariableSpec {
                    display_name: if col.new_name.trim().is_empty() {
                        col.source_header.clone()
                    } else {
                        col.new_name.clone()
                    },
                    xml_name: None,
                    kind: VariableKind::LinkedFile,
                    required: false,
                    mapped_template_var: None,
                }),
                3 => {
                    if let Some(var) = existing_vars.get(col.existing_var_index.max(0) as usize) {
                        ImportColumnTarget::ExistingVariable(var.id.clone())
                    } else {
                        ImportColumnTarget::Ignore
                    }
                }
                _ => ImportColumnTarget::Ignore,
            };
            ImportColumnMapping {
                source_col_index: col.source_index,
                target,
            }
        })
        .collect()
}
