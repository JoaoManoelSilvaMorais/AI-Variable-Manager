use std::path::Path;
use std::sync::Arc;

use crate::domain::ports::{ExportSummary, ProjectRepository};
use crate::domain::project::PathStrategy;
use crate::domain::xml_model::{ParsedXmlTemplate, PrefixUpdateReport};
use crate::shared::error::{AppError, AppResult};

pub struct ExportService {
    _repo: Arc<dyn ProjectRepository>,
}

impl ExportService {
    pub fn new(repo: Arc<dyn ProjectRepository>) -> Self {
        Self { _repo: repo }
    }

    pub fn inspect_xml_template(&self, _xml_raw: &str) -> AppResult<ParsedXmlTemplate> {
        Err(AppError::not_implemented(
            "ExportService::inspect_xml_template será implementado na M4",
        ))
    }

    pub fn bind_xml_template(
        &self,
        _project_id: &str,
        _template_path: Option<&Path>,
        _xml_raw: &str,
    ) -> AppResult<ParsedXmlTemplate> {
        Err(AppError::not_implemented(
            "ExportService::bind_xml_template será implementado na M4",
        ))
    }

    pub fn generate_xml_preview(&self, _project_id: &str) -> AppResult<String> {
        Err(AppError::not_implemented(
            "ExportService::generate_xml_preview será implementado na M4",
        ))
    }

    pub fn validate_xml_structure(
        &self,
        _xml_raw: &str,
        _expected_datasets: usize,
    ) -> AppResult<()> {
        Err(AppError::not_implemented(
            "ExportService::validate_xml_structure será implementado na M4",
        ))
    }

    pub fn export_package(
        &self,
        _project_id: &str,
        _target_dir: &Path,
        _overwrite_confirmed: bool,
    ) -> AppResult<ExportSummary> {
        Err(AppError::not_implemented(
            "ExportService::export_package será implementado na M4",
        ))
    }

    pub fn update_exported_prefix(
        &self,
        _export_dir: &Path,
        _new_strategy: PathStrategy,
    ) -> AppResult<PrefixUpdateReport> {
        Err(AppError::not_implemented(
            "ExportService::update_exported_prefix será implementado na M4",
        ))
    }
}
