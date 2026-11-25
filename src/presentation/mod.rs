pub mod app_controller;
pub mod dialogs;
pub mod grid_model;
pub mod photo_model;

pub mod generated {
    slint::include_modules!();
}

pub use app_controller::AppController;
pub use generated::{
    AppWindow, AuditIssueItem, GridCellItem, GridRowItem, ImportColumnItem,
    ImportPreviewRowItem, Palette, PhotoAssetItem, PhotoCandidateItem, PhotoCellItem,
    ProjectSummaryItem, TemplateVarItem, ThemeTokens, VariableItem,
};
pub use slint::language::ColorScheme;
