use std::path::Path;
use std::sync::Arc;

use crate::domain::ports::{ImportColumnMapping, ImportMode, ProjectRepository, RawTable};
use crate::shared::error::{AppError, AppResult};

pub struct ImportService {
    _repo: Arc<dyn ProjectRepository>,
}

impl ImportService {
    pub fn new(repo: Arc<dyn ProjectRepository>) -> Self {
        Self { _repo: repo }
    }

    pub fn list_sheets(&self, _path: &Path) -> AppResult<Vec<String>> {
        Err(AppError::not_implemented(
            "ImportService::list_sheets será implementado na M2",
        ))
    }

    pub fn preview_spreadsheet(
        &self,
        _path: &Path,
        _sheet: Option<&str>,
        _max_rows: Option<usize>,
    ) -> AppResult<RawTable> {
        Err(AppError::not_implemented(
            "ImportService::preview_spreadsheet será implementado na M2",
        ))
    }

    pub fn commit_import(
        &self,
        _project_id: &str,
        _path: &Path,
        _sheet: Option<&str>,
        _header_row: usize,
        _mappings: &[ImportColumnMapping],
        _mode: ImportMode,
    ) -> AppResult<usize> {
        Err(AppError::not_implemented(
            "ImportService::commit_import será implementado na M2",
        ))
    }
}
