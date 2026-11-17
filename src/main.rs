#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ai_variable_manager::application::AppServices;
use ai_variable_manager::domain::ThemePreference;
use ai_variable_manager::presentation::{AppWindow, ColorScheme, Palette};
use directories::BaseDirs;
use slint::{ComponentHandle, Global};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::MakeWriterExt;

fn app_root_dir() -> PathBuf {
    BaseDirs::new()
        .map(|b| b.data_local_dir().to_path_buf())
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join("AI Variable Manager")
}

fn init_logging(logs_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(logs_dir)?;
    let log_path = logs_dir.join("ai-variable-manager.log");
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ai_variable_manager=debug"));

    #[cfg(debug_assertions)]
    {
        let writer = std::io::stdout.and(Mutex::new(file));
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(writer)
            .init();
    }

    #[cfg(not(debug_assertions))]
    {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(Mutex::new(file))
            .init();
    }

    info!(log_file = %log_path.display(), "Sistema de diagnóstico inicializado");
    Ok(())
}

fn apply_theme(app: &AppWindow, pref: ThemePreference) {
    let scheme = match pref {
        ThemePreference::Light => ColorScheme::Light,
        ThemePreference::Dark => ColorScheme::Dark,
        ThemePreference::System => ColorScheme::Unknown,
    };
    Palette::get(app).set_color_scheme(scheme);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = app_root_dir();
    let logs_dir = root.join("logs");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir)?;

    if let Err(e) = init_logging(&logs_dir) {
        eprintln!(
            "Aviso: falha ao iniciar log em {}: {e}",
            logs_dir.display()
        );
    }

    let db_path = data_dir.join("aivm.db");
    let services = AppServices::new_with_db_path(&db_path)?;
    info!(db_path = %db_path.display(), "Banco SQLite inicializado com sucesso");

    let app = AppWindow::new()?;

    let initial_pref = services
        .project_service
        .get_theme_preference()
        .unwrap_or(ThemePreference::System);
    apply_theme(&app, initial_pref);

    let is_dark = matches!(Palette::get(&app).get_color_scheme(), ColorScheme::Dark);
    app.set_dark_mode(is_dark);

    let app_weak = app.as_weak();
    let project_service = services.project_service.clone();
    app.on_toggle_theme_requested(move || {
        let Some(ui) = app_weak.upgrade() else {
            return;
        };
        let palette = Palette::get(&ui);
        let next_pref = match palette.get_color_scheme() {
            ColorScheme::Dark => ThemePreference::Light,
            _ => ThemePreference::Dark,
        };
        apply_theme(&ui, next_pref);
        ui.set_dark_mode(matches!(next_pref, ThemePreference::Dark));
        if let Err(e) = project_service.set_theme_preference(next_pref) {
            warn!(error = %e, "Falha ao persistir preferência de tema");
        }
    });

    if let Err(e) = app.run() {
        error!(error = %e, "Erro na execução da janela Slint");
        return Err(e.into());
    }
    Ok(())
}
