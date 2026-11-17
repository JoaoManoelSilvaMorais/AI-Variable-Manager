use std::sync::Arc;

use crate::domain::ports::ProjectRepository;
use crate::domain::record::{CellDiagnostic, GridWindow, Record};
use crate::shared::error::{AppError, AppResult};

pub struct DataService {
    _repo: Arc<dyn ProjectRepository>,
}

impl DataService {
    pub fn new(repo: Arc<dyn ProjectRepository>) -> Self {
        Self { _repo: repo }
    }

    pub fn add_record(&self, _project_id: &str, _dataset_name: Option<&str>) -> AppResult<Record> {
        Err(AppError::not_implemented(
            "DataService::add_record será implementado na M2",
        ))
    }

    pub fn update_cell(
        &self,
        _project_id: &str,
        _record_id: &str,
        _variable_id: &str,
        _value: &str,
    ) -> AppResult<()> {
        Err(AppError::not_implemented(
            "DataService::update_cell será implementado na M2",
        ))
    }

    pub fn update_dataset_name(
        &self,
        _project_id: &str,
        _record_id: &str,
        _dataset_name: &str,
    ) -> AppResult<()> {
        Err(AppError::not_implemented(
            "DataService::update_dataset_name será implementado na M2",
        ))
    }

    pub fn delete_records(&self, _project_id: &str, _record_ids: &[String]) -> AppResult<()> {
        Err(AppError::not_implemented(
            "DataService::delete_records será implementado na M2",
        ))
    }

    pub fn reorder_records(&self, _project_id: &str, _ordered_ids: &[String]) -> AppResult<()> {
        Err(AppError::not_implemented(
            "DataService::reorder_records será implementado na M2",
        ))
    }

    pub fn get_records_window(
        &self,
        _project_id: &str,
        _offset: usize,
        _limit: usize,
    ) -> AppResult<GridWindow> {
        Err(AppError::not_implemented(
            "DataService::get_records_window será implementado na M2",
        ))
    }

    pub fn paste_block(
        &self,
        _project_id: &str,
        _start_row: usize,
        _start_col: usize,
        _clipboard_text: &str,
    ) -> AppResult<usize> {
        Err(AppError::not_implemented(
            "DataService::paste_block será implementado na M2",
        ))
    }

    pub fn compute_cell_diagnostics(&self, _project_id: &str) -> AppResult<Vec<CellDiagnostic>> {
        Err(AppError::not_implemented(
            "DataService::compute_cell_diagnostics será implementado na M2",
        ))
    }
}
