use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationSeverity {
    BlockingError,
    Warning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ValidationCode {
    E01EmptyProject,
    E02InvalidOrDuplicateXmlName,
    E03DuplicateDatasetName,
    E04RequiredTextEmpty,
    E05RequiredPhotoMissing,
    E06LinkedAssetMissingOnDisk,
    E07UnsupportedAssetFormat,
    E08MissingOrInvalidXmlTemplate,
    E09TemplateMappingConflict,
    E10UnresolvedAmbiguousPhoto,
    W01OptionalCellEmpty,
    W02SuggestedPhotoPending,
    W03DuplicateKeyTextValue,
    W04SharedAssetAcrossRecords,
    W05UnmatchedScannedPhotos,
    W06TargetFilesExist,
    W07IgnoredTemplateVariableType,
}

impl ValidationCode {
    pub fn code_str(&self) -> &'static str {
        match self {
            Self::E01EmptyProject => "E01",
            Self::E02InvalidOrDuplicateXmlName => "E02",
            Self::E03DuplicateDatasetName => "E03",
            Self::E04RequiredTextEmpty => "E04",
            Self::E05RequiredPhotoMissing => "E05",
            Self::E06LinkedAssetMissingOnDisk => "E06",
            Self::E07UnsupportedAssetFormat => "E07",
            Self::E08MissingOrInvalidXmlTemplate => "E08",
            Self::E09TemplateMappingConflict => "E09",
            Self::E10UnresolvedAmbiguousPhoto => "E10",
            Self::W01OptionalCellEmpty => "W01",
            Self::W02SuggestedPhotoPending => "W02",
            Self::W03DuplicateKeyTextValue => "W03",
            Self::W04SharedAssetAcrossRecords => "W04",
            Self::W05UnmatchedScannedPhotos => "W05",
            Self::W06TargetFilesExist => "W06",
            Self::W07IgnoredTemplateVariableType => "W07",
        }
    }

    pub fn default_severity(&self) -> ValidationSeverity {
        match self {
            Self::E01EmptyProject
            | Self::E02InvalidOrDuplicateXmlName
            | Self::E03DuplicateDatasetName
            | Self::E04RequiredTextEmpty
            | Self::E05RequiredPhotoMissing
            | Self::E06LinkedAssetMissingOnDisk
            | Self::E07UnsupportedAssetFormat
            | Self::E08MissingOrInvalidXmlTemplate
            | Self::E09TemplateMappingConflict
            | Self::E10UnresolvedAmbiguousPhoto => ValidationSeverity::BlockingError,
            Self::W01OptionalCellEmpty
            | Self::W02SuggestedPhotoPending
            | Self::W03DuplicateKeyTextValue
            | Self::W04SharedAssetAcrossRecords
            | Self::W05UnmatchedScannedPhotos
            | Self::W06TargetFilesExist
            | Self::W07IgnoredTemplateVariableType => ValidationSeverity::Warning,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationTarget {
    Project,
    Variable { variable_id: String },
    Record { record_id: String },
    Cell { record_id: String, variable_id: String },
    Asset { asset_id: String },
    ExportTarget { path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub code: ValidationCode,
    pub severity: ValidationSeverity,
    pub title: String,
    pub message: String,
    pub target: ValidationTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AuditReport {
    pub issues: Vec<ValidationIssue>,
    pub blocking_count: usize,
    pub warning_count: usize,
    pub can_export: bool,
}

impl AuditReport {
    pub fn from_issues(issues: Vec<ValidationIssue>) -> Self {
        let blocking_count = issues
            .iter()
            .filter(|i| i.severity == ValidationSeverity::BlockingError)
            .count();
        let warning_count = issues
            .iter()
            .filter(|i| i.severity == ValidationSeverity::Warning)
            .count();
        Self {
            issues,
            blocking_count,
            warning_count,
            can_export: blocking_count == 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_report_allows_export_only_when_zero_blocking_errors() {
        let warn_only = AuditReport::from_issues(vec![ValidationIssue {
            code: ValidationCode::W01OptionalCellEmpty,
            severity: ValidationSeverity::Warning,
            title: "Célula opcional vazia".into(),
            message: "Campo opcional sem preenchimento.".into(),
            target: ValidationTarget::Project,
        }]);
        assert!(warn_only.can_export);
        assert_eq!(warn_only.blocking_count, 0);
        assert_eq!(warn_only.warning_count, 1);

        let with_error = AuditReport::from_issues(vec![
            warn_only.issues[0].clone(),
            ValidationIssue {
                code: ValidationCode::E04RequiredTextEmpty,
                severity: ValidationSeverity::BlockingError,
                title: "Campo obrigatório vazio".into(),
                message: "Campo obrigatório não preenchido.".into(),
                target: ValidationTarget::Project,
            },
        ]);
        assert!(!with_error.can_export);
        assert_eq!(with_error.blocking_count, 1);
        assert_eq!(with_error.warning_count, 1);
    }
}
