use std::path::Path;
use std::sync::Arc;

use crate::domain::ports::ProjectRepository;
use crate::domain::validation::AuditReport;
use crate::shared::error::{AppError, AppResult};

pub struct ValidationService {
    _repo: Arc<dyn ProjectRepository>,
}

impl ValidationService {
    pub fn new(repo: Arc<dyn ProjectRepository>) -> Self {
        Self { _repo: repo }
    }

    pub fn audit_project(&self, _project_id: &str) -> AppResult<AuditReport> {
        Err(AppError::not_implemented(
            "ValidationService::audit_project será implementado na M4",
        ))
    }

    pub fn audit_project_with_target(
        &self,
        _project_id: &str,
        _target_dir: Option<&Path>,
    ) -> AppResult<AuditReport> {
        Err(AppError::not_implemented(
            "ValidationService::audit_project_with_target será implementado na M4",
        ))
    }
}
