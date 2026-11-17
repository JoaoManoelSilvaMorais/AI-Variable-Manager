use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::domain::asset::{Asset, ImageFormat, MatchCandidate, ScanResult};
use crate::domain::project::{PathStrategy, Project, ProjectMode, ProjectSnapshot, ProjectSummary};
use crate::domain::record::{Record, RecordValue};
use crate::domain::variable::{NewVariableSpec, Variable};
use crate::domain::xml_model::{ParsedXmlTemplate, PrefixUpdateReport, XmlExportSpec};
use crate::shared::error::AppResult;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportColumnTarget {
    ExistingVariable(String),
    NewVariable(NewVariableSpec),
    Ignore,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportColumnMapping {
    pub source_col_index: usize,
    pub target: ImportColumnTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportMode {
    Append,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawTable {
    pub sheet_name: Option<String>,
    pub rows: Vec<Vec<String>>,
    pub detected_delimiter: Option<char>,
    pub detected_encoding: Option<String>,
    pub total_rows_in_source: usize,
    pub precision_warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailData {
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThumbnailState {
    Ready(Arc<ThumbnailData>),
    Loading,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportAssetCopy {
    pub asset_id: String,
    pub source_path: PathBuf,
    pub target_rel_path: String,
    pub format: ImageFormat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportReportRow {
    pub record_id: String,
    pub dataset_name: String,
    pub status: String,
    pub mapped_assets: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportPlan {
    pub project_id: String,
    pub project_name: String,
    pub target_dir: PathBuf,
    pub path_strategy: PathStrategy,
    pub xml_content: String,
    pub expected_datasets: usize,
    pub asset_copies: Vec<ExportAssetCopy>,
    pub report_rows: Vec<ExportReportRow>,
    pub audit_warnings_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportSummary {
    pub target_dir: PathBuf,
    pub dados_xml_path: PathBuf,
    pub images_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub report_csv_path: PathBuf,
    pub exported_datasets: usize,
    pub exported_assets: usize,
    pub total_asset_bytes: u64,
}

pub trait ProjectRepository: Send + Sync {
    fn create_project(&self, name: &str, mode: ProjectMode) -> AppResult<Project>;
    fn open_project(&self, project_id: &str) -> AppResult<ProjectSnapshot>;
    fn list_projects(&self) -> AppResult<Vec<ProjectSummary>>;
    fn rename_project(&self, project_id: &str, new_name: &str) -> AppResult<()>;
    fn delete_project(&self, project_id: &str) -> AppResult<()>;
    fn update_project_settings(&self, project: &Project) -> AppResult<()>;
    fn upsert_variable(&self, var: &Variable) -> AppResult<()>;
    fn duplicate_variable(&self, project_id: &str, variable_id: &str) -> AppResult<Variable>;
    fn delete_variable(&self, project_id: &str, variable_id: &str) -> AppResult<()>;
    fn reorder_variables(&self, project_id: &str, ordered_ids: &[String]) -> AppResult<()>;
    fn upsert_records_and_values(
        &self,
        project_id: &str,
        records: &[Record],
        values: &[RecordValue],
    ) -> AppResult<()>;
    fn reorder_records(&self, project_id: &str, ordered_ids: &[String]) -> AppResult<()>;
    fn delete_records(&self, project_id: &str, record_ids: &[String]) -> AppResult<()>;
    fn replace_assets(&self, project_id: &str, assets: &[Asset]) -> AppResult<()>;
    fn update_match_candidates(
        &self,
        project_id: &str,
        candidates: &[MatchCandidate],
    ) -> AppResult<()>;
    fn get_setting(&self, key: &str) -> AppResult<Option<String>>;
    fn set_setting(&self, key: &str, value: &str) -> AppResult<()>;
}

pub trait SpreadsheetReader: Send + Sync {
    fn list_sheets(&self, path: &Path) -> AppResult<Vec<String>>;
    fn read_table(
        &self,
        path: &Path,
        sheet: Option<&str>,
        max_rows: Option<usize>,
    ) -> AppResult<RawTable>;
}

pub trait PhotoScanner: Send + Sync {
    fn scan_directory(&self, root: &Path) -> AppResult<ScanResult>;
}

pub trait ThumbnailCache: Send + Sync {
    fn get_or_request(&self, asset_id: &str, path: &Path) -> ThumbnailState;
    fn clear(&self);
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub trait XmlEngine: Send + Sync {
    fn inspect_template(&self, xml_raw: &str) -> AppResult<ParsedXmlTemplate>;
    fn generate_xml(&self, spec: &XmlExportSpec) -> AppResult<String>;
    fn validate_xml_structure(&self, xml_raw: &str, expected_datasets: usize) -> AppResult<()>;
    fn update_export_prefix(
        &self,
        export_dir: &Path,
        new_strategy: PathStrategy,
    ) -> AppResult<PrefixUpdateReport>;
}

pub trait PackageExporter: Send + Sync {
    fn check_existing_targets(&self, target_dir: &Path) -> AppResult<Vec<PathBuf>>;
    fn export_atomic(
        &self,
        plan: &ExportPlan,
        overwrite_confirmed: bool,
    ) -> AppResult<ExportSummary>;
}
