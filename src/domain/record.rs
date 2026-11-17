use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::shared::error::{AppError, AppResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchStatus {
    #[default]
    Unmatched,
    AutoExact,
    AutoSuggested,
    Ambiguous,
    Manual,
}

impl MatchStatus {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Self::Unmatched => "unmatched",
            Self::AutoExact => "auto_exact",
            Self::AutoSuggested => "auto_suggested",
            Self::Ambiguous => "ambiguous",
            Self::Manual => "manual",
        }
    }

    pub fn from_db_str(raw: &str) -> AppResult<Self> {
        match raw {
            "unmatched" => Ok(Self::Unmatched),
            "auto_exact" => Ok(Self::AutoExact),
            "auto_suggested" => Ok(Self::AutoSuggested),
            "ambiguous" => Ok(Self::Ambiguous),
            "manual" => Ok(Self::Manual),
            other => Err(AppError::validation(format!(
                "Status de correspondência inválido: '{other}'"
            ))),
        }
    }

    pub fn display_label_pt_br(&self) -> &'static str {
        match self {
            Self::Unmatched => "Sem foto",
            Self::AutoExact => "Automática (exata)",
            Self::AutoSuggested => "Sugestão pendente",
            Self::Ambiguous => "Ambígua (requer confirmação)",
            Self::Manual => "Manual",
        }
    }

    pub fn is_confirmed_link(&self) -> bool {
        matches!(self, Self::AutoExact | Self::Manual)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub project_id: String,
    pub dataset_name: String,
    pub sort_order: usize,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordValue {
    pub record_id: String,
    pub variable_id: String,
    pub text_value: String,
    pub asset_id: Option<String>,
    pub match_status: MatchStatus,
    pub match_confidence: Option<f64>,
}

impl RecordValue {
    pub fn new_text(
        record_id: impl Into<String>,
        variable_id: impl Into<String>,
        text_value: impl Into<String>,
    ) -> Self {
        Self {
            record_id: record_id.into(),
            variable_id: variable_id.into(),
            text_value: text_value.into(),
            asset_id: None,
            match_status: MatchStatus::Unmatched,
            match_confidence: None,
        }
    }

    pub fn new_unmatched(record_id: impl Into<String>, variable_id: impl Into<String>) -> Self {
        Self::new_text(record_id, variable_id, String::new())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellDiagnosticKind {
    EmptyRequired,
    EmptyOptional,
    DuplicateValue,
    MissingAssetLink,
    MissingAssetOnDisk,
    AmbiguousPhotoMatch,
    SuggestedPhotoPending,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellDiagnostic {
    pub record_id: String,
    pub variable_id: String,
    pub kind: CellDiagnosticKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GridWindow {
    pub total_records: usize,
    pub offset: usize,
    pub records: Vec<Record>,
    pub values: Vec<RecordValue>,
}

pub fn compute_reordered_record_ids(
    existing: &[Record],
    ordered_ids: &[String],
) -> AppResult<Vec<String>> {
    if existing.len() != ordered_ids.len() {
        return Err(AppError::validation(
            "A lista de reordenação não corresponde ao número total de registros do projeto.",
        ));
    }

    let existing_set: HashSet<&str> = existing.iter().map(|r| r.id.as_str()).collect();
    let mut seen: HashSet<&str> = HashSet::with_capacity(ordered_ids.len());

    for id in ordered_ids {
        if !existing_set.contains(id.as_str()) {
            return Err(AppError::validation(format!(
                "Registro desconhecido na reordenação: '{id}'."
            )));
        }
        if !seen.insert(id.as_str()) {
            return Err(AppError::validation(format!(
                "Registro duplicado na lista de reordenação: '{id}'."
            )));
        }
    }

    Ok(ordered_ids.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record(id: &str, sort_order: usize) -> Record {
        Record {
            id: id.to_string(),
            project_id: "proj-1".to_string(),
            dataset_name: format!("DS_{sort_order}"),
            sort_order,
            created_at: "2026-11-17T00:00:00.000Z".to_string(),
            updated_at: "2026-11-17T00:00:00.000Z".to_string(),
        }
    }

    #[test]
    fn compute_reordered_record_ids_validates_permutations() {
        let records = vec![
            sample_record("r0", 0),
            sample_record("r1", 1),
            sample_record("r2", 2),
        ];

        assert_eq!(
            compute_reordered_record_ids(&records, &["r2".into(), "r0".into(), "r1".into()])
                .unwrap(),
            vec!["r2", "r0", "r1"]
        );
        assert!(
            compute_reordered_record_ids(&records, &["r0".into(), "r0".into(), "r1".into()])
                .is_err()
        );
        assert!(compute_reordered_record_ids(&records, &["r0".into(), "r1".into()]).is_err());
        assert!(
            compute_reordered_record_ids(&records, &["r0".into(), "r1".into(), "rx".into()])
                .is_err()
        );
    }
}
