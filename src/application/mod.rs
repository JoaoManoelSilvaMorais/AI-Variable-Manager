pub mod data_service;
pub mod export_service;
pub mod import_service;
pub mod photo_service;
pub mod project_service;
pub mod validation_service;

use std::path::Path;
use std::sync::Arc;

pub use data_service::DataService;
pub use export_service::ExportService;
pub use import_service::ImportService;
pub use photo_service::PhotoService;
pub use project_service::ProjectService;
pub use validation_service::ValidationService;

use crate::domain::ports::ProjectRepository;
use crate::infrastructure::sqlite::{
    SqliteConnectionManager, SqliteProjectRepository, migrations,
};
use crate::shared::error::AppResult;

#[derive(Clone)]
pub struct AppServices {
    pub project_repo: Arc<dyn ProjectRepository>,
    pub project_service: Arc<ProjectService>,
    pub data_service: Arc<DataService>,
    pub import_service: Arc<ImportService>,
    pub photo_service: Arc<PhotoService>,
    pub validation_service: Arc<ValidationService>,
    pub export_service: Arc<ExportService>,
}

impl AppServices {
    pub fn new_with_db_path(db_path: &Path) -> AppResult<Self> {
        let conn_mgr = SqliteConnectionManager::open(db_path)?;
        conn_mgr.with_conn_mut(migrations::run_migrations)?;
        let project_repo: Arc<dyn ProjectRepository> =
            Arc::new(SqliteProjectRepository::new(conn_mgr));

        Ok(Self {
            project_service: Arc::new(ProjectService::new(Arc::clone(&project_repo))),
            data_service: Arc::new(DataService::new(Arc::clone(&project_repo))),
            import_service: Arc::new(ImportService::new(Arc::clone(&project_repo))),
            photo_service: Arc::new(PhotoService::new(Arc::clone(&project_repo))),
            validation_service: Arc::new(ValidationService::new(Arc::clone(&project_repo))),
            export_service: Arc::new(ExportService::new(Arc::clone(&project_repo))),
            project_repo,
        })
    }
}
