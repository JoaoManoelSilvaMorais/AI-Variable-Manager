use std::path::Path;
use std::sync::Arc;

use crate::domain::asset::{BatchLinkResult, PhotoDiagnostics, ScanResult, UnmatchedPool};
use crate::domain::ports::{ProjectRepository, ThumbnailState};
use crate::shared::error::{AppError, AppResult};

pub struct PhotoService {
    _repo: Arc<dyn ProjectRepository>,
}

impl PhotoService {
    pub fn new(repo: Arc<dyn ProjectRepository>) -> Self {
        Self { _repo: repo }
    }

    pub fn scan_directory(&self, _project_id: &str, _root_dir: &Path) -> AppResult<ScanResult> {
        Err(AppError::not_implemented(
            "PhotoService::scan_directory será implementado na M3",
        ))
    }

    pub fn run_auto_match(&self, _project_id: &str) -> AppResult<PhotoDiagnostics> {
        Err(AppError::not_implemented(
            "PhotoService::run_auto_match será implementado na M3",
        ))
    }

    pub fn accept_match_candidate(
        &self,
        _project_id: &str,
        _record_id: &str,
        _variable_id: &str,
        _asset_id: &str,
    ) -> AppResult<()> {
        Err(AppError::not_implemented(
            "PhotoService::accept_match_candidate será implementado na M3",
        ))
    }

    pub fn link_manual(
        &self,
        _project_id: &str,
        _record_id: &str,
        _variable_id: &str,
        _image_path: &Path,
    ) -> AppResult<()> {
        Err(AppError::not_implemented(
            "PhotoService::link_manual será implementado na M3",
        ))
    }

    pub fn unlink_photo(
        &self,
        _project_id: &str,
        _record_id: &str,
        _variable_id: &str,
    ) -> AppResult<()> {
        Err(AppError::not_implemented(
            "PhotoService::unlink_photo será implementado na M3",
        ))
    }

    pub fn get_unmatched_pool(
        &self,
        _project_id: &str,
        _variable_id: &str,
    ) -> AppResult<UnmatchedPool> {
        Err(AppError::not_implemented(
            "PhotoService::get_unmatched_pool será implementado na M3",
        ))
    }

    pub fn batch_link(
        &self,
        _project_id: &str,
        _pairs: &[(String, String, String)],
        _overwrite_existing: bool,
    ) -> AppResult<BatchLinkResult> {
        Err(AppError::not_implemented(
            "PhotoService::batch_link será implementado na M3",
        ))
    }

    pub fn compute_diagnostics(
        &self,
        _project_id: &str,
        _refresh_disk_existence: bool,
    ) -> AppResult<PhotoDiagnostics> {
        Err(AppError::not_implemented(
            "PhotoService::compute_diagnostics será implementado na M3",
        ))
    }

    pub fn get_or_request_thumbnail(
        &self,
        _asset_id: &str,
        _path: &Path,
    ) -> AppResult<ThumbnailState> {
        Err(AppError::not_implemented(
            "PhotoService::get_or_request_thumbnail será implementado na M3",
        ))
    }

    pub fn clear_thumbnail_cache(&self) -> AppResult<()> {
        Err(AppError::not_implemented(
            "PhotoService::clear_thumbnail_cache será implementado na M3",
        ))
    }

    pub fn thumbnail_cache_len(&self) -> AppResult<usize> {
        Err(AppError::not_implemented(
            "PhotoService::thumbnail_cache_len será implementado na M3",
        ))
    }
}
