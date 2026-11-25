#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::Arc;

use slint::ComponentHandle;
use tracing_subscriber::EnvFilter;

use ai_variable_manager::AppServices;
use ai_variable_manager::presentation::{AppController, AppWindow};

fn resolve_app_data_dir() -> PathBuf {
    if let Some(proj_dirs) = directories::ProjectDirs::from("", "", "AI Variable Manager") {
        let local_data = proj_dirs.data_local_dir();
        if local_data.file_name().is_some_and(|n| n == "data")
            && let Some(parent) = local_data.parent()
        {
            return parent.to_path_buf();
        }
        return local_data.to_path_buf();
    }
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local_app_data).join("AI Variable Manager");
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("aivm-data")
}

fn init_logging(log_dir: &std::path::Path) {
    let _ = std::fs::create_dir_all(log_dir);
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("ai_variable_manager=info,warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .try_init();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base_dir = resolve_app_data_dir();
    let log_dir = base_dir.join("logs");
    let data_dir = base_dir.join("data");

    init_logging(&log_dir);
    std::fs::create_dir_all(&data_dir)?;

    let db_path = data_dir.join("aivm.db");
    let services = Arc::new(AppServices::new_with_db_path(&db_path)?);
    let app = AppWindow::new()?;

    AppController::bootstrap(&app, services, db_path);

    app.run()?;
    Ok(())
}
