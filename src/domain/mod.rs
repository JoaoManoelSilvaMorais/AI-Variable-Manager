pub mod asset;
pub mod ports;
pub mod project;
pub mod record;
pub mod validation;
pub mod variable;
pub mod xml_model;

pub use asset::{
    Asset, BatchLinkResult, DuplicateAssetAssignment, ImageFormat, MatchCandidate,
    PhotoDiagnostics, ScanResult, ScannedAsset, UnmatchedPool, REASON_AMBIGUOUS_TIE,
    REASON_EXACT_NORMALIZED, REASON_FUZZY_STEM,
};
pub use ports::{
    ExportAssetCopy, ExportPlan, ExportReportRow, ExportSummary, ImportColumnMapping,
    ImportColumnTarget, ImportMode, PackageExporter, PhotoScanner, ProjectRepository, RawTable,
    SpreadsheetReader, ThumbnailCache, ThumbnailData, ThumbnailState, XmlEngine,
};
pub use project::{
    validate_project_name, PathStrategy, Project, ProjectMode, ProjectSnapshot, ProjectSummary,
    ThemePreference,
};
pub use record::{
    compute_reordered_record_ids, CellDiagnostic, CellDiagnosticKind, GridWindow, MatchStatus,
    Record, RecordValue,
};
pub use validation::{
    AuditReport, ValidationCode, ValidationIssue, ValidationSeverity, ValidationTarget,
};
pub use variable::{
    build_duplicated_variable, compute_reordered_ids, ensure_unique_xml_name,
    validate_display_name, validate_xml_name, NewVariableSpec, Variable, VariableKind,
};
pub use xml_model::{
    ParsedXmlTemplate, PrefixUpdateReport, TemplateVariable, TemplateVariableTrait, XmlCellValue,
    XmlColumnBinding, XmlDatasetEntry, XmlExportSpec,
};
