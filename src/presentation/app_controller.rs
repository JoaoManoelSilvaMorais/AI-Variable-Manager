use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use slint::{ComponentHandle, Global, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::application::AppServices;
use crate::domain::ports::ImportMode;
use crate::domain::project::{ProjectMode, ProjectSnapshot, ThemePreference};
use crate::domain::validation::{ValidationSeverity, ValidationTarget};
use crate::domain::variable::VariableKind;
use crate::domain::xml_model::TemplateVariableTrait;
use crate::presentation::dialogs::{
    pick_folder, pick_image_file, pick_spreadsheet_file, pick_xml_template_file,
    read_clipboard_text,
};
use crate::presentation::grid_model::{
    GridViewState, GRID_PAGE_SIZE, build_existing_var_names, build_grid_page,
    build_import_column_models, build_variable_items, compile_import_mappings,
    rebuild_import_columns,
};
use crate::presentation::photo_model::{PhotoViewState, build_photo_tab_bundle, resolve_thumb_if_loaded};
use crate::presentation::{
    AppWindow, AuditIssueItem, ColorScheme, Palette, ProjectSummaryItem, TemplateVarItem,
    ThemeTokens,
};
use crate::shared::error::{AppError, AppResult, DomainError};

#[derive(Debug, Clone)]
enum PendingConfirm {
    DeleteProject {
        project_id: String,
    },
    DeleteRecords {
        project_id: String,
        record_ids: Vec<String>,
    },
    DeleteVariable {
        project_id: String,
        variable_id: String,
    },
    OverwriteExport {
        project_id: String,
        target_dir: PathBuf,
    },
}

pub struct AppController {
    services: Arc<AppServices>,
    db_path: PathBuf,
    active_project_id: Option<String>,
    grid_state: GridViewState,
    photo_state: PhotoViewState,
    pending_confirm: Option<PendingConfirm>,
    thumb_poll_timer: Timer,
}

impl AppController {
    pub fn bootstrap(app: &AppWindow, services: Arc<AppServices>, db_path: PathBuf) {
        let controller = Rc::new(RefCell::new(Self {
            services,
            db_path,
            active_project_id: None,
            grid_state: GridViewState::default(),
            photo_state: PhotoViewState::default(),
            pending_confirm: None,
            thumb_poll_timer: Timer::default(),
        }));

        {
            let mut ctrl = controller.borrow_mut();
            ctrl.init_startup(app);
        }

        Self::wire_callbacks(app, Rc::clone(&controller));
    }

    fn init_startup(&mut self, app: &AppWindow) {
        app.set_app_version_label(env!("CARGO_PKG_VERSION").into());
        app.set_db_path_label(self.db_path.to_string_lossy().as_ref().into());
        app.set_storage_hint(format!("SQLite: {}", self.db_path.display()).into());

        match self.services.project_service.get_theme_preference() {
            Ok(pref) => apply_theme(app, pref),
            Err(err) => self.show_error(app, err),
        }

        if let Ok(Some(last_id)) = self.services.project_service.get_last_opened_project_id() {
            if self.open_project_internal(app, &last_id, false).is_err() {
                self.active_project_id = None;
            }
        }

        self.refresh_projects_list(app);
        self.refresh_settings_metrics(app);
        self.update_tab_header(app);
    }

    fn wire_callbacks(app: &AppWindow, controller: Rc<RefCell<Self>>) {
        // Tab navigation
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(&controller);
            app.on_tab_selected(move |tab| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    if tab > 0 && tab < 4 && c.active_project_id.is_none() {
                        c.show_banner(&app, 2, "Abra ou crie um projeto antes de acessar esta aba.");
                        app.set_active_tab(0);
                    } else {
                        app.set_active_tab(tab);
                        if tab == 1 {
                            c.refresh_grid_only(&app);
                        } else if tab == 2 {
                            c.refresh_photos_tab(&app, true);
                        } else if tab == 3 {
                            c.refresh_xml_export_tab(&app);
                        } else if tab == 4 {
                            c.refresh_settings_metrics(&app);
                        }
                    }
                    c.update_tab_header(&app);
                }
            });
        }

        // Theme switching
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(&controller);
            app.on_theme_changed(move |mode_idx| {
                if let Some(app) = weak.upgrade() {
                    let pref = index_to_theme(mode_idx);
                    apply_theme(&app, pref);
                    let mut c = ctrl.borrow_mut();
                    if let Err(err) = c.services.project_service.set_theme_preference(pref) {
                        c.show_error(&app, err);
                    }
                }
            });
        }

        {
            let weak = app.as_weak();
            app.on_dismiss_banner(move || {
                if let Some(app) = weak.upgrade() {
                    app.set_banner_kind(0);
                    app.set_banner_text("".into());
                }
            });
        }

        // Confirm dialog
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(&controller);
            app.on_dialog_cancelled(move || {
                if let Some(app) = weak.upgrade() {
                    app.set_confirm_open(false);
                    ctrl.borrow_mut().pending_confirm = None;
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(&controller);
            app.on_dialog_confirmed(move || {
                if let Some(app) = weak.upgrade() {
                    app.set_confirm_open(false);
                    let action = ctrl.borrow_mut().pending_confirm.take();
                    if let Some(act) = action {
                        ctrl.borrow_mut().execute_confirmed_action(&app, act);
                    }
                }
            });
        }

        // Projects callbacks
        Self::wire_project_callbacks(app, &controller);
        Self::wire_grid_callbacks(app, &controller);
        Self::wire_import_callbacks(app, &controller);
        Self::wire_photo_callbacks(app, &controller);
        Self::wire_xml_export_callbacks(app, &controller);

        // Settings callbacks
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(&controller);
            app.on_clear_thumbnail_cache(move || {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    if let Err(err) = c.services.photo_service.clear_thumbnail_cache() {
                        c.show_error(&app, err);
                    } else {
                        c.refresh_settings_metrics(&app);
                        c.show_banner(&app, 1, "Cache LRU de miniaturas limpo.");
                    }
                }
            });
        }
    }

    fn wire_project_callbacks(app: &AppWindow, controller: &Rc<RefCell<Self>>) {
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_create_project(move |name, mode_idx| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    let mode = if mode_idx == 1 {
                        ProjectMode::WithTemplate
                    } else {
                        ProjectMode::WithoutTemplate
                    };
                    match c.services.project_service.create_project(name.as_str(), mode) {
                        Ok(proj) => {
                            app.set_new_project_name("".into());
                            let _ = c.open_project_internal(&app, &proj.id, true);
                            c.refresh_projects_list(&app);
                            c.show_banner(&app, 1, &format!("Projeto '{}' criado.", proj.name));
                        }
                        Err(err) => c.show_error(&app, err),
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_open_project(move |id| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    match c.open_project_internal(&app, id.as_str(), true) {
                        Ok(()) => {
                            c.refresh_projects_list(&app);
                            c.clear_banner(&app);
                        }
                        Err(err) => c.show_error(&app, err),
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            app.on_start_rename_project(move |id, current_name| {
                if let Some(app) = weak.upgrade() {
                    app.set_rename_target_id(id);
                    app.set_rename_draft_name(current_name);
                }
            });
        }
        {
            let weak = app.as_weak();
            app.on_cancel_rename_project(move || {
                if let Some(app) = weak.upgrade() {
                    app.set_rename_target_id("".into());
                    app.set_rename_draft_name("".into());
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_commit_rename_project(move |id, new_name| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    match c
                        .services
                        .project_service
                        .rename_project(id.as_str(), new_name.as_str())
                    {
                        Ok(()) => {
                            app.set_rename_target_id("".into());
                            app.set_rename_draft_name("".into());
                            c.refresh_projects_list(&app);
                            if c.active_project_id.as_deref() == Some(id.as_str()) {
                                c.refresh_all_active_project(&app);
                            }
                            c.show_banner(&app, 1, "Nome do projeto atualizado.");
                        }
                        Err(err) => c.show_error(&app, err),
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_request_delete_project(move |id, name| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    c.open_confirm_dialog(
                        &app,
                        "Excluir Projeto",
                        &format!(
                            "Deseja realmente excluir o projeto '{name}'?\nOs arquivos de planilhas, fotos e XMLs originais na sua máquina NÃO serão apagados."
                        ),
                        "Excluir Projeto",
                        true,
                        PendingConfirm::DeleteProject {
                            project_id: id.into(),
                        },
                    );
                }
            });
        }
    }

    fn wire_grid_callbacks(app: &AppWindow, controller: &Rc<RefCell<Self>>) {
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_add_record(move || {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    let Some(pid) = c.active_project_id.clone() else {
                        return;
                    };
                    match c.services.data_service.add_record(&pid, None) {
                        Ok(_) => {
                            let total_pages = c.current_total_pages(&pid);
                            c.grid_state.current_page = total_pages;
                            c.refresh_all_active_project(&app);
                        }
                        Err(err) => c.show_error(&app, err),
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_toggle_row_selection(move |record_id| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    let rid = record_id.to_string();
                    if !c.grid_state.selected_record_ids.remove(&rid) {
                        c.grid_state.selected_record_ids.insert(rid);
                    }
                    c.refresh_grid_only(&app);
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_move_record_up(move |record_id| {
                if let Some(app) = weak.upgrade() {
                    ctrl.borrow_mut().nudge_record(&app, record_id.as_str(), -1);
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_move_record_down(move |record_id| {
                if let Some(app) = weak.upgrade() {
                    ctrl.borrow_mut().nudge_record(&app, record_id.as_str(), 1);
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_request_delete_selected_records(move || {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    let Some(pid) = c.active_project_id.clone() else {
                        return;
                    };
                    if c.grid_state.selected_record_ids.is_empty() {
                        return;
                    }
                    let ids: Vec<String> = c.grid_state.selected_record_ids.iter().cloned().collect();
                    let count = ids.len();
                    c.open_confirm_dialog(
                        &app,
                        "Excluir Registros Selecionados",
                        &format!("Confirma a exclusão de {count} registro(s) selecionado(s) da tabela?"),
                        "Excluir Registros",
                        true,
                        PendingConfirm::DeleteRecords {
                            project_id: pid,
                            record_ids: ids,
                        },
                    );
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_dataset_name_edited(move |record_id, val| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    let Some(pid) = c.active_project_id.clone() else {
                        return;
                    };
                    if let Err(err) = c
                        .services
                        .data_service
                        .update_dataset_name(&pid, record_id.as_str(), val.as_str())
                    {
                        c.show_error(&app, err);
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_cell_edited(move |record_id, var_id, val| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    let Some(pid) = c.active_project_id.clone() else {
                        return;
                    };
                    if let Err(err) = c.services.data_service.update_cell(
                        &pid,
                        record_id.as_str(),
                        var_id.as_str(),
                        val.as_str(),
                    ) {
                        c.show_error(&app, err);
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_refresh_diagnostics(move || {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    c.refresh_all_active_project(&app);
                    c.show_banner(&app, 1, "Diagnósticos da tabela atualizados.");
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_pick_cell_photo(move |record_id, var_id| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    let Some(pid) = c.active_project_id.clone() else {
                        return;
                    };
                    let start_dir = c
                        .services
                        .project_service
                        .open_project(&pid)
                        .ok()
                        .and_then(|s| s.project.photo_root_dir);
                    if let Some(img_path) = pick_image_file(start_dir.as_deref()) {
                        match c.services.photo_service.link_manual(
                            &pid,
                            record_id.as_str(),
                            var_id.as_str(),
                            &img_path,
                        ) {
                            Ok(()) => {
                                c.refresh_all_active_project(&app);
                                c.show_banner(&app, 1, "Fotografia vinculada ao registro.");
                            }
                            Err(err) => c.show_error(&app, err),
                        }
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_page_prev(move || {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    if c.grid_state.current_page > 1 {
                        c.grid_state.current_page -= 1;
                        c.refresh_grid_only(&app);
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_page_next(move || {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    c.grid_state.current_page += 1;
                    c.refresh_grid_only(&app);
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_paste_os_clipboard(move |row_1based, col_idx| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    match read_clipboard_text() {
                        Ok(txt) => {
                            app.set_paste_draft(txt.as_str().into());
                            c.apply_paste_block(&app, row_1based, col_idx, &txt);
                        }
                        Err(err) => c.show_error(&app, err),
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_paste_manual_block(move |row_1based, col_idx, txt| {
                if let Some(app) = weak.upgrade() {
                    ctrl.borrow_mut()
                        .apply_paste_block(&app, row_1based, col_idx, txt.as_str());
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_create_variable(move |name, kind_idx, required| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    let Some(pid) = c.active_project_id.clone() else {
                        return;
                    };
                    let kind = if kind_idx == 1 {
                        VariableKind::LinkedFile
                    } else {
                        VariableKind::Text
                    };
                    match c.services.project_service.create_variable(
                        &pid,
                        name.as_str(),
                        None,
                        kind,
                        required,
                    ) {
                        Ok(v) => {
                            app.set_new_var_name("".into());
                            c.refresh_all_active_project(&app);
                            c.show_banner(&app, 1, &format!("Coluna '{}' adicionada.", v.display_name));
                        }
                        Err(err) => c.show_error(&app, err),
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_move_variable(move |var_id, delta| {
                if let Some(app) = weak.upgrade() {
                    ctrl.borrow_mut()
                        .nudge_variable(&app, var_id.as_str(), delta);
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_toggle_variable_required(move |var_id, required| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    if let Some(pid) = c.active_project_id.clone()
                        && let Ok(snap) = c.services.project_service.open_project(&pid)
                        && let Some(var) = snap.variable_by_id(var_id.as_str())
                    {
                        match c.services.project_service.update_variable_options(
                            &pid,
                            &var.id,
                            var.kind,
                            required,
                            var.mapped_template_var.clone(),
                        ) {
                            Ok(_) => c.refresh_all_active_project(&app),
                            Err(err) => c.show_error(&app, err),
                        }
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_toggle_variable_kind(move |var_id, is_photo| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    if let Some(pid) = c.active_project_id.clone()
                        && let Ok(snap) = c.services.project_service.open_project(&pid)
                        && let Some(var) = snap.variable_by_id(var_id.as_str())
                    {
                        let kind = if is_photo {
                            VariableKind::LinkedFile
                        } else {
                            VariableKind::Text
                        };
                        match c.services.project_service.update_variable_options(
                            &pid,
                            &var.id,
                            kind,
                            var.required,
                            var.mapped_template_var.clone(),
                        ) {
                            Ok(_) => c.refresh_all_active_project(&app),
                            Err(err) => c.show_error(&app, err),
                        }
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            app.on_start_rename_variable(move |var_id, display, xml| {
                if let Some(app) = weak.upgrade() {
                    app.set_edit_var_id(var_id);
                    app.set_edit_var_display(display);
                    app.set_edit_var_xml(xml);
                }
            });
        }
        {
            let weak = app.as_weak();
            app.on_cancel_rename_variable(move || {
                if let Some(app) = weak.upgrade() {
                    app.set_edit_var_id("".into());
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_commit_rename_variable(move |var_id, display, xml| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    let Some(pid) = c.active_project_id.clone() else {
                        return;
                    };
                    let xml_opt = if xml.trim().is_empty() {
                        None
                    } else {
                        Some(xml.as_str())
                    };
                    match c.services.project_service.rename_variable(
                        &pid,
                        var_id.as_str(),
                        display.as_str(),
                        xml_opt,
                    ) {
                        Ok(_) => {
                            app.set_edit_var_id("".into());
                            c.refresh_all_active_project(&app);
                            c.show_banner(&app, 1, "Coluna renomeada.");
                        }
                        Err(err) => c.show_error(&app, err),
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_duplicate_variable(move |var_id| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    let Some(pid) = c.active_project_id.clone() else {
                        return;
                    };
                    match c
                        .services
                        .project_service
                        .duplicate_variable(&pid, var_id.as_str())
                    {
                        Ok(v) => {
                            c.refresh_all_active_project(&app);
                            c.show_banner(&app, 1, &format!("Coluna '{}' duplicada.", v.display_name));
                        }
                        Err(err) => c.show_error(&app, err),
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_request_delete_variable(move |var_id, name| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    let Some(pid) = c.active_project_id.clone() else {
                        return;
                    };
                    c.open_confirm_dialog(
                        &app,
                        "Excluir Coluna",
                        &format!(
                            "Deseja realmente excluir a coluna '{name}' e todos os seus valores neste projeto?"
                        ),
                        "Excluir Coluna",
                        true,
                        PendingConfirm::DeleteVariable {
                            project_id: pid,
                            variable_id: var_id.into(),
                        },
                    );
                }
            });
        }
    }

    fn wire_import_callbacks(app: &AppWindow, controller: &Rc<RefCell<Self>>) {
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_open_import_file(move || {
                if let Some(app) = weak.upgrade() {
                    if let Some(path) = pick_spreadsheet_file() {
                        ctrl.borrow_mut().start_import_wizard(&app, path);
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_close_import_wizard(move || {
                if let Some(app) = weak.upgrade() {
                    ctrl.borrow_mut().grid_state.import.open = false;
                    app.set_show_import_wizard(false);
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_import_sheet_changed(move |sheet_idx| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    c.grid_state.import.selected_sheet_index = sheet_idx.max(0) as usize;
                    c.reload_import_preview(&app);
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_import_header_row_changed(move |header_1based| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    c.grid_state.import.header_row_1based = header_1based.max(1) as usize;
                    app.set_import_header_row_1based(c.grid_state.import.header_row_1based as i32);
                    c.rebuild_import_columns_state(&app);
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_import_column_target_changed(move |src_idx, mode| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    if let Some(col) = c
                        .grid_state
                        .import
                        .columns
                        .iter_mut()
                        .find(|col| col.source_index == src_idx.max(0) as usize)
                    {
                        col.target_mode = mode;
                    }
                    app.set_import_columns(build_import_column_models(&c.grid_state.import.columns));
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_import_column_new_name_changed(move |src_idx, name| {
                if let Some(_app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    if let Some(col) = c
                        .grid_state
                        .import
                        .columns
                        .iter_mut()
                        .find(|col| col.source_index == src_idx.max(0) as usize)
                    {
                        col.new_name = name.into();
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_import_column_existing_changed(move |src_idx, existing_idx| {
                if let Some(_app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    if let Some(col) = c
                        .grid_state
                        .import
                        .columns
                        .iter_mut()
                        .find(|col| col.source_index == src_idx.max(0) as usize)
                    {
                        col.existing_var_index = existing_idx;
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_commit_import(move || {
                if let Some(app) = weak.upgrade() {
                    ctrl.borrow_mut().commit_import_wizard(&app);
                }
            });
        }
    }

    fn wire_photo_callbacks(app: &AppWindow, controller: &Rc<RefCell<Self>>) {
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_pick_photo_folder(move || {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    let Some(pid) = c.active_project_id.clone() else {
                        return;
                    };
                    let current_dir = c
                        .services
                        .project_service
                        .open_project(&pid)
                        .ok()
                        .and_then(|s| s.project.photo_root_dir);
                    if let Some(folder) = pick_folder("Selecionar pasta de fotografias", current_dir.as_deref()) {
                        c.scan_photo_folder_internal(&app, &pid, &folder);
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_rescan_photo_folder(move || {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    let Some(pid) = c.active_project_id.clone() else {
                        return;
                    };
                    if let Ok(snap) = c.services.project_service.open_project(&pid)
                        && let Some(dir) = snap.project.photo_root_dir
                    {
                        c.scan_photo_folder_internal(&app, &pid, &dir);
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_run_auto_match(move || {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    let Some(pid) = c.active_project_id.clone() else {
                        return;
                    };
                    match c.services.photo_service.run_auto_match(&pid) {
                        Ok(diag) => {
                            c.refresh_all_active_project(&app);
                            c.show_banner(
                                &app,
                                1,
                                &format!(
                                    "Match automático concluído: {} confirmada(s), {} sugestão(ões), {} ambígua(s), {} sem foto.",
                                    diag.matched_count,
                                    diag.suggested_count,
                                    diag.ambiguous_count,
                                    diag.records_missing_photo.len()
                                ),
                            );
                        }
                        Err(err) => c.show_error(&app, err),
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_photo_filter_changed(move |idx| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    c.photo_state.filter_index = idx.max(0) as usize;
                    c.refresh_photos_tab(&app, true);
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_photo_column_changed(move |idx| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    c.photo_state.selected_photo_col_index = idx.max(0) as usize;
                    c.photo_state.selected_cell_index = None;
                    c.refresh_photos_tab(&app, true);
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_select_photo_cell(move |idx| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    c.photo_state.selected_cell_index = if idx >= 0 { Some(idx as usize) } else { None };
                    c.refresh_photos_tab(&app, true);
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_toggle_batch_cell(move |idx| {
                if let Some(app) = weak.upgrade() {
                    if let Some(cell) = app.get_photo_cells().row_data(idx.max(0) as usize) {
                        let mut c = ctrl.borrow_mut();
                        let rid = cell.record_id.to_string();
                        if !c.photo_state.batch_selected_record_ids.remove(&rid) {
                            c.photo_state.batch_selected_record_ids.insert(rid);
                        }
                        c.refresh_photos_tab(&app, false);
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_pick_manual_photo(move |idx| {
                if let Some(app) = weak.upgrade()
                    && let Some(cell) = app.get_photo_cells().row_data(idx.max(0) as usize)
                {
                    let mut c = ctrl.borrow_mut();
                    let Some(pid) = c.active_project_id.clone() else {
                        return;
                    };
                    let start_dir = c
                        .services
                        .project_service
                        .open_project(&pid)
                        .ok()
                        .and_then(|s| s.project.photo_root_dir);
                    if let Some(img_path) = pick_image_file(start_dir.as_deref()) {
                        match c.services.photo_service.link_manual(
                            &pid,
                            cell.record_id.as_str(),
                            cell.variable_id.as_str(),
                            &img_path,
                        ) {
                            Ok(()) => {
                                c.refresh_all_active_project(&app);
                                c.show_banner(&app, 1, "Vínculo manual salvo.");
                            }
                            Err(err) => c.show_error(&app, err),
                        }
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_unlink_photo(move |idx| {
                if let Some(app) = weak.upgrade()
                    && let Some(cell) = app.get_photo_cells().row_data(idx.max(0) as usize)
                {
                    let mut c = ctrl.borrow_mut();
                    let Some(pid) = c.active_project_id.clone() else {
                        return;
                    };
                    match c.services.photo_service.unlink_photo(
                        &pid,
                        cell.record_id.as_str(),
                        cell.variable_id.as_str(),
                    ) {
                        Ok(()) => {
                            c.refresh_all_active_project(&app);
                            c.show_banner(&app, 1, "Vínculo de foto removido.");
                        }
                        Err(err) => c.show_error(&app, err),
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_accept_candidate(move |cell_idx, asset_id| {
                if let Some(app) = weak.upgrade()
                    && let Some(cell) = app.get_photo_cells().row_data(cell_idx.max(0) as usize)
                {
                    let mut c = ctrl.borrow_mut();
                    let Some(pid) = c.active_project_id.clone() else {
                        return;
                    };
                    match c.services.photo_service.accept_match_candidate(
                        &pid,
                        cell.record_id.as_str(),
                        cell.variable_id.as_str(),
                        asset_id.as_str(),
                    ) {
                        Ok(()) => {
                            c.refresh_all_active_project(&app);
                            c.show_banner(&app, 1, "Sugestão confirmada manualmente.");
                        }
                        Err(err) => c.show_error(&app, err),
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_request_cell_thumb(move |idx| {
                if let Some(app) = weak.upgrade() {
                    ctrl.borrow_mut().request_cell_thumbnail(&app, idx);
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_request_candidate_thumb(move |idx| {
                if let Some(app) = weak.upgrade() {
                    ctrl.borrow_mut().request_candidate_thumbnail(&app, idx);
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_request_asset_thumb(move |idx| {
                if let Some(app) = weak.upgrade() {
                    ctrl.borrow_mut().request_asset_thumbnail(&app, idx);
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_select_pool_asset(move |idx| {
                if let Some(app) = weak.upgrade() {
                    ctrl.borrow_mut().photo_state.selected_pool_asset_index =
                        if idx >= 0 { Some(idx as usize) } else { None };
                    app.set_selected_pool_asset_index(idx);
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_pool_filter_changed(move |txt| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    c.photo_state.pool_filter_text = txt.into();
                    c.refresh_photos_tab(&app, true);
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_link_selected_pool_asset(move || {
                if let Some(app) = weak.upgrade() {
                    ctrl.borrow_mut().batch_link_action(&app, false);
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_batch_link_selected_to_pool(move || {
                if let Some(app) = weak.upgrade() {
                    ctrl.borrow_mut().batch_link_action(&app, true);
                }
            });
        }
    }

    fn wire_xml_export_callbacks(app: &AppWindow, controller: &Rc<RefCell<Self>>) {
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_switch_project_mode(move |mode_idx| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    let Some(pid) = c.active_project_id.clone() else {
                        return;
                    };
                    if let Ok(mut snap) = c.services.project_service.open_project(&pid) {
                        snap.project.mode = if mode_idx == 1 {
                            ProjectMode::WithTemplate
                        } else {
                            ProjectMode::WithoutTemplate
                        };
                        match c.services.project_service.update_project_settings(&snap.project) {
                            Ok(()) => {
                                c.refresh_all_active_project(&app);
                                c.refresh_projects_list(&app);
                            }
                            Err(err) => c.show_error(&app, err),
                        }
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_pick_and_bind_xml_template(move || {
                if let Some(app) = weak.upgrade()
                    && let Some(path) = pick_xml_template_file()
                {
                    let mut c = ctrl.borrow_mut();
                    let Some(pid) = c.active_project_id.clone() else {
                        return;
                    };
                    match std::fs::read_to_string(&path) {
                        Ok(raw) => match c.services.export_service.bind_xml_template(&pid, Some(&path), &raw) {
                            Ok(parsed) => {
                                c.refresh_all_active_project(&app);
                                c.show_banner(
                                    &app,
                                    1,
                                    &format!(
                                        "XML-base '{}' vinculado ({} variáveis detectadas).",
                                        path.file_name().unwrap_or_default().to_string_lossy(),
                                        parsed.variables.len()
                                    ),
                                );
                            }
                            Err(err) => c.show_error(&app, err),
                        },
                        Err(err) => c.show_error(&app, AppError::io("ler arquivo XML-base", err)),
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_template_mapping_changed(move |tpl_var_name, col_option_idx| {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    let Some(pid) = c.active_project_id.clone() else {
                        return;
                    };
                    let Ok(snap) = c.services.project_service.open_project(&pid) else {
                        return;
                    };
                    // Option 0 is "(Sem vínculo)"; 1..=N correspond to snap.variables[i-1]
                    for v in &snap.variables {
                        let currently_mapped_here =
                            v.mapped_template_var.as_deref() == Some(tpl_var_name.as_str());
                        if currently_mapped_here {
                            let _ = c.services.project_service.update_variable_options(
                                &pid,
                                &v.id,
                                v.kind,
                                v.required,
                                None,
                            );
                        }
                    }
                    if col_option_idx > 0
                        && let Some(target_var) = snap.variables.get((col_option_idx - 1) as usize)
                    {
                        if let Err(err) = c.services.project_service.update_variable_options(
                            &pid,
                            &target_var.id,
                            target_var.kind,
                            target_var.required,
                            Some(tpl_var_name.into()),
                        ) {
                            c.show_error(&app, err);
                            return;
                        }
                    }
                    c.refresh_all_active_project(&app);
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_run_audit(move || {
                if let Some(app) = weak.upgrade() {
                    let mut c = ctrl.borrow_mut();
                    c.refresh_xml_export_tab(&app);
                    c.show_banner(&app, 1, "Auditoria pré-exportação atualizada.");
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_click_audit_issue(move |target_kind, rec_id, var_id| {
                if let Some(app) = weak.upgrade() {
                    ctrl.borrow_mut().navigate_to_audit_issue(
                        &app,
                        target_kind.as_str(),
                        rec_id.as_str(),
                        var_id.as_str(),
                    );
                }
            });
        }
        {
            let weak = app.as_weak();
            app.on_pick_export_dir(move || {
                if let Some(app) = weak.upgrade()
                    && let Some(dir) = pick_folder("Selecionar pasta de saída do pacote", None)
                {
                    app.set_export_target_dir(dir.to_string_lossy().as_ref().into());
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_export_package(move || {
                if let Some(app) = weak.upgrade() {
                    let target_str = app.get_export_target_dir().to_string();
                    let mut c = ctrl.borrow_mut();
                    let Some(pid) = c.active_project_id.clone() else {
                        return;
                    };
                    c.run_export(&app, &pid, PathBuf::from(target_str), false);
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_refresh_xml_preview(move || {
                if let Some(app) = weak.upgrade() {
                    let target_str = app.get_export_target_dir().to_string();
                    let target_dir = if target_str.trim().is_empty() {
                        None
                    } else {
                        Some(PathBuf::from(target_str.trim()))
                    };
                    let mut c = ctrl.borrow_mut();
                    let Some(pid) = c.active_project_id.clone() else {
                        return;
                    };
                    match c
                        .services
                        .export_service
                        .generate_xml_preview_for_dir(&pid, target_dir.as_deref())
                    {
                        Ok(xml) => app.set_xml_preview_text(xml.into()),
                        Err(err) => c.show_error(&app, err),
                    }
                }
            });
        }
        {
            let weak = app.as_weak();
            app.on_pick_prefix_update_dir(move || {
                if let Some(app) = weak.upgrade()
                    && let Some(dir) = pick_folder("Selecionar pasta exportada já existente", None)
                {
                    app.set_prefix_update_dir(dir.to_string_lossy().as_ref().into());
                }
            });
        }
        {
            let weak = app.as_weak();
            let ctrl = Rc::clone(controller);
            app.on_run_prefix_update(move || {
                if let Some(app) = weak.upgrade() {
                    let dir = PathBuf::from(app.get_prefix_update_dir().to_string());
                    let mut c = ctrl.borrow_mut();
                    match c.services.export_service.update_exported_prefix(&dir) {
                        Ok(rep) => {
                            c.show_banner(
                                &app,
                                1,
                                &format!(
                                    "dados.xml atualizado com sucesso: {} referência(s) reescrita(s), {} asset(s) verificado(s).",
                                    rep.updated_references, rep.verified_assets
                                ),
                            );
                        }
                        Err(err) => c.show_error(&app, err),
                    }
                }
            });
        }
    }

    fn execute_confirmed_action(&mut self, app: &AppWindow, action: PendingConfirm) {
        match action {
            PendingConfirm::DeleteProject { project_id } => {
                match self.services.project_service.delete_project(&project_id) {
                    Ok(()) => {
                        if self.active_project_id.as_deref() == Some(project_id.as_str()) {
                            self.active_project_id = None;
                            app.set_has_active_project(false);
                            app.set_active_project_name("".into());
                            app.set_active_project_mode("".into());
                            app.set_active_tab(0);
                        }
                        self.refresh_projects_list(app);
                        self.show_banner(app, 1, "Projeto excluído com segurança.");
                    }
                    Err(err) => self.show_error(app, err),
                }
            }
            PendingConfirm::DeleteRecords {
                project_id,
                record_ids,
            } => match self.services.data_service.delete_records(&project_id, &record_ids) {
                Ok(()) => {
                    self.grid_state.selected_record_ids.clear();
                    self.refresh_all_active_project(app);
                    self.show_banner(app, 1, "Registros removidos.");
                }
                Err(err) => self.show_error(app, err),
            },
            PendingConfirm::DeleteVariable {
                project_id,
                variable_id,
            } => match self
                .services
                .project_service
                .delete_variable(&project_id, &variable_id)
            {
                Ok(()) => {
                    self.refresh_all_active_project(app);
                    self.show_banner(app, 1, "Coluna removida.");
                }
                Err(err) => self.show_error(app, err),
            },
            PendingConfirm::OverwriteExport {
                project_id,
                target_dir,
            } => {
                self.run_export(app, &project_id, target_dir, true);
            }
        }
    }

    fn open_project_internal(
        &mut self,
        app: &AppWindow,
        project_id: &str,
        switch_to_grid_tab: bool,
    ) -> AppResult<()> {
        let snap = self.services.project_service.open_project(project_id)?;
        self.active_project_id = Some(snap.project.id.clone());
        self.grid_state = GridViewState::default();
        self.photo_state = PhotoViewState::default();

        if let Some(ref last_dir) = snap.project.last_export_dir {
            app.set_export_target_dir(last_dir.to_string_lossy().as_ref().into());
        } else {
            app.set_export_target_dir("".into());
        }
        app.set_last_export_summary("".into());
        app.set_xml_preview_text("".into());

        if switch_to_grid_tab {
            app.set_active_tab(1);
        }
        self.refresh_with_snapshot(app, &snap);
        Ok(())
    }

    fn refresh_projects_list(&mut self, app: &AppWindow) {
        match self.services.project_service.list_projects() {
            Ok(list) => {
                let items: Vec<ProjectSummaryItem> = list
                    .into_iter()
                    .map(|p| ProjectSummaryItem {
                        is_active: self.active_project_id.as_deref() == Some(p.id.as_str()),
                        id: p.id.into(),
                        name: p.name.into(),
                        mode_label: p.mode.display_label_pt_br().into(),
                        record_count: p.record_count as i32,
                        variable_count: p.variable_count as i32,
                        updated_at: p.updated_at.into(),
                    })
                    .collect();
                app.set_projects(ModelRc::new(Rc::new(VecModel::from(items))));
            }
            Err(err) => self.show_error(app, err),
        }
    }

    fn refresh_all_active_project(&mut self, app: &AppWindow) {
        let Some(ref pid) = self.active_project_id.clone() else {
            return;
        };
        match self.services.project_service.open_project(pid) {
            Ok(snap) => self.refresh_with_snapshot(app, &snap),
            Err(err) => self.show_error(app, err),
        }
    }

    fn refresh_with_snapshot(&mut self, app: &AppWindow, snap: &ProjectSnapshot) {
        app.set_has_active_project(true);
        app.set_active_project_name(snap.project.name.as_str().into());
        app.set_active_project_mode(snap.project.mode.display_label_pt_br().into());
        app.set_is_template_mode(snap.project.mode == ProjectMode::WithTemplate);

        app.set_variables(build_variable_items(&snap.variables));
        app.set_existing_var_names(build_existing_var_names(&snap.variables));

        self.refresh_grid_with_snap(app, snap);
        self.refresh_photos_with_snap(app, snap, app.get_active_tab() == 2);
        self.refresh_xml_export_with_snap(app, snap);
        self.refresh_settings_metrics(app);
        self.update_tab_header(app);
    }

    fn refresh_grid_only(&mut self, app: &AppWindow) {
        let Some(ref pid) = self.active_project_id.clone() else {
            return;
        };
        if let Ok(snap) = self.services.project_service.open_project(pid) {
            self.refresh_grid_with_snap(app, &snap);
        }
    }

    fn refresh_grid_with_snap(&mut self, app: &AppWindow, snap: &ProjectSnapshot) {
        match build_grid_page(&self.services, snap, &mut self.grid_state) {
            Ok((rows, total_records, total_pages)) => {
                app.set_grid_rows(rows);
                app.set_total_records(total_records as i32);
                app.set_current_page(self.grid_state.current_page as i32);
                app.set_total_pages(total_pages as i32);
                app.set_selected_records_count(self.grid_state.selected_record_ids.len() as i32);
            }
            Err(err) => self.show_error(app, err),
        }
    }

    fn refresh_photos_tab(&mut self, app: &AppWindow, auto_request_visible: bool) {
        let Some(ref pid) = self.active_project_id.clone() else {
            return;
        };
        if let Ok(snap) = self.services.project_service.open_project(pid) {
            self.refresh_photos_with_snap(app, &snap, auto_request_visible);
        }
    }

    fn refresh_photos_with_snap(
        &mut self,
        app: &AppWindow,
        snap: &ProjectSnapshot,
        auto_request_visible: bool,
    ) {
        app.set_photo_root_dir(
            snap.project
                .photo_root_dir
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
                .into(),
        );
        match build_photo_tab_bundle(&self.services, snap, &mut self.photo_state, auto_request_visible) {
            Ok(bundle) => {
                app.set_photo_column_names(bundle.photo_col_names);
                app.set_selected_photo_col_index(self.photo_state.selected_photo_col_index as i32);
                app.set_photo_total_assets(bundle.diagnostics.total_scanned_assets as i32);
                app.set_photo_matched_count(bundle.diagnostics.matched_count as i32);
                app.set_photo_suggested_count(bundle.diagnostics.suggested_count as i32);
                app.set_photo_ambiguous_count(bundle.diagnostics.ambiguous_count as i32);
                app.set_photo_missing_count(bundle.diagnostics.records_missing_photo.len() as i32);
                app.set_photo_unmatched_assets_count(bundle.diagnostics.unmatched_asset_ids.len() as i32);
                app.set_photo_duplicate_warnings_count(
                    bundle.diagnostics.duplicate_asset_assignments.len() as i32,
                );
                app.set_photo_missing_on_disk_count(
                    bundle.diagnostics.missing_on_disk_asset_ids.len() as i32,
                );

                app.set_photo_cells(ModelRc::new(Rc::new(VecModel::from(bundle.cells))));
                app.set_selected_photo_cell_index(
                    self.photo_state.selected_cell_index.map(|i| i as i32).unwrap_or(-1),
                );
                app.set_active_photo_candidates(ModelRc::new(Rc::new(VecModel::from(bundle.candidates))));
                app.set_unmatched_photo_assets(ModelRc::new(Rc::new(VecModel::from(
                    bundle.unmatched_assets,
                ))));
                app.set_selected_pool_asset_index(
                    self.photo_state
                        .selected_pool_asset_index
                        .map(|i| i as i32)
                        .unwrap_or(-1),
                );

                if bundle.has_loading_thumbs {
                    self.ensure_thumb_timer(app);
                }
            }
            Err(err) => self.show_error(app, err),
        }
    }

    fn ensure_thumb_timer(&self, app: &AppWindow) {
        let weak = app.as_weak();
        let services = Arc::clone(&self.services);
        self.thumb_poll_timer
            .start(TimerMode::Repeated, Duration::from_millis(85), move || {
                let Some(app) = weak.upgrade() else {
                    return;
                };
                if app.get_active_tab() != 2 {
                    return;
                }
                let mut still_loading = false;

                let cell_model = app.get_photo_cells();
                for idx in 0..cell_model.row_count() {
                    if let Some(mut item) = cell_model.row_data(idx)
                        && !item.has_thumb
                        && !item.linked_path.is_empty()
                    {
                        let p = PathBuf::from(item.linked_path.as_str());
                        let asset_key = item.linked_path.to_string();
                        let (ready, loading, img) = resolve_thumb_if_loaded(&services, &asset_key, &p, true);
                        if ready {
                            item.has_thumb = true;
                            item.thumb = img;
                            cell_model.set_row_data(idx, item);
                        }
                        still_loading |= loading;
                    }
                }

                let cand_model = app.get_active_photo_candidates();
                for idx in 0..cand_model.row_count() {
                    if let Some(mut item) = cand_model.row_data(idx)
                        && !item.has_thumb
                    {
                        let p = PathBuf::from(item.full_path.as_str());
                        let (ready, loading, img) =
                            resolve_thumb_if_loaded(&services, item.asset_id.as_str(), &p, true);
                        if ready {
                            item.has_thumb = true;
                            item.thumb = img;
                            cand_model.set_row_data(idx, item);
                        }
                        still_loading |= loading;
                    }
                }

                let asset_model = app.get_unmatched_photo_assets();
                for idx in 0..asset_model.row_count() {
                    if let Some(mut item) = asset_model.row_data(idx)
                        && !item.has_thumb
                    {
                        let p = PathBuf::from(item.full_path.as_str());
                        let (ready, loading, img) =
                            resolve_thumb_if_loaded(&services, item.asset_id.as_str(), &p, true);
                        if ready {
                            item.has_thumb = true;
                            item.thumb = img;
                            asset_model.set_row_data(idx, item);
                        }
                        still_loading |= loading;
                    }
                }

                app.set_thumbnail_cache_count(
                    services.photo_service.thumbnail_cache_len().unwrap_or(0) as i32,
                );
                let _ = still_loading;
            });
    }

    fn request_cell_thumbnail(&self, app: &AppWindow, idx: i32) {
        let model = app.get_photo_cells();
        let uidx = idx.max(0) as usize;
        if let Some(mut item) = model.row_data(uidx)
            && !item.has_thumb
            && !item.linked_path.is_empty()
        {
            let path = PathBuf::from(item.linked_path.as_str());
            let (ready, loading, img) =
                resolve_thumb_if_loaded(&self.services, item.linked_path.as_str(), &path, true);
            if ready {
                item.has_thumb = true;
                item.thumb = img;
                model.set_row_data(uidx, item);
            } else if loading {
                self.ensure_thumb_timer(app);
            }
        }
    }

    fn request_candidate_thumbnail(&self, app: &AppWindow, idx: i32) {
        let model = app.get_active_photo_candidates();
        let uidx = idx.max(0) as usize;
        if let Some(mut item) = model.row_data(uidx)
            && !item.has_thumb
        {
            let path = PathBuf::from(item.full_path.as_str());
            let (ready, loading, img) =
                resolve_thumb_if_loaded(&self.services, item.asset_id.as_str(), &path, true);
            if ready {
                item.has_thumb = true;
                item.thumb = img;
                model.set_row_data(uidx, item);
            } else if loading {
                self.ensure_thumb_timer(app);
            }
        }
    }

    fn request_asset_thumbnail(&self, app: &AppWindow, idx: i32) {
        let model = app.get_unmatched_photo_assets();
        let uidx = idx.max(0) as usize;
        if let Some(mut item) = model.row_data(uidx)
            && !item.has_thumb
        {
            let path = PathBuf::from(item.full_path.as_str());
            let (ready, loading, img) =
                resolve_thumb_if_loaded(&self.services, item.asset_id.as_str(), &path, true);
            if ready {
                item.has_thumb = true;
                item.thumb = img;
                model.set_row_data(uidx, item);
            } else if loading {
                self.ensure_thumb_timer(app);
            }
        }
    }

    fn refresh_xml_export_tab(&mut self, app: &AppWindow) {
        let Some(ref pid) = self.active_project_id.clone() else {
            return;
        };
        if let Ok(snap) = self.services.project_service.open_project(pid) {
            self.refresh_xml_export_with_snap(app, &snap);
        }
    }

    fn refresh_xml_export_with_snap(&mut self, app: &AppWindow, snap: &ProjectSnapshot) {
        let mut var_options = vec![SharedString::from("(Sem vínculo)")];
        for v in &snap.variables {
            var_options.push(SharedString::from(format!(
                "{} [{}]",
                v.display_name,
                v.kind.display_label_pt_br()
            )));
        }
        app.set_project_var_options(ModelRc::new(Rc::new(VecModel::from(var_options))));

        app.set_template_path_label(
            snap.project
                .xml_template_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
                .into(),
        );

        let mut tpl_items = Vec::new();
        if let Some(ref tpl_raw) = snap.project.xml_template_raw
            && let Ok(parsed) = self.services.export_service.inspect_xml_template(tpl_raw)
        {
            app.set_template_var_set_name(
                parsed.var_set_name.as_deref().unwrap_or("(padrão)").into(),
            );
            app.set_template_datasets_count(parsed.sample_datasets_count as i32);
            app.set_template_has_c2pa(parsed.has_c2pa_metadata);

            for tv in &parsed.variables {
                let is_ignored = matches!(
                    tv.trait_kind,
                    TemplateVariableTrait::Visibility
                        | TemplateVariableTrait::GraphData
                        | TemplateVariableTrait::Unknown(_)
                );
                let trait_label = match &tv.trait_kind {
                    TemplateVariableTrait::TextContent => "Texto (textcontent)",
                    TemplateVariableTrait::Fileref => "Imagem (fileref)",
                    TemplateVariableTrait::Visibility => "Visibilidade",
                    TemplateVariableTrait::GraphData => "Gráfico",
                    TemplateVariableTrait::Unknown(u) => u.as_str(),
                };
                let wrapper_label = match tv.sample_uses_p_wrapper {
                    Some(true) => "com <p>",
                    Some(false) => "sem <p>",
                    None => "padrão",
                };
                let mapped_idx = snap
                    .variables
                    .iter()
                    .position(|v| {
                        v.mapped_template_var.as_deref() == Some(tv.var_name.as_str())
                            || (v.mapped_template_var.is_none()
                                && v.xml_name == tv.var_name)
                    })
                    .map(|i| (i + 1) as i32)
                    .unwrap_or(0);

                tpl_items.push(TemplateVarItem {
                    var_name: tv.var_name.as_str().into(),
                    trait_label: trait_label.into(),
                    wrapper_label: wrapper_label.into(),
                    is_ignored,
                    mapped_col_index: mapped_idx,
                });
            }
        } else {
            app.set_template_var_set_name("".into());
            app.set_template_datasets_count(0);
            app.set_template_has_c2pa(false);
        }
        app.set_template_vars(ModelRc::new(Rc::new(VecModel::from(tpl_items))));

        let target_opt = {
            let s = app.get_export_target_dir().to_string();
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(PathBuf::from(trimmed))
            }
        };

        match self
            .services
            .validation_service
            .audit_project_with_target(&snap.project.id, target_opt.as_deref())
        {
            Ok(audit) => {
                app.set_can_export(audit.can_export);
                app.set_audit_blocking_count(audit.blocking_count as i32);
                app.set_audit_warning_count(audit.warning_count as i32);

                let issues: Vec<AuditIssueItem> = audit
                    .issues
                    .iter()
                    .map(|iss| {
                        let (target_kind, rec_id, var_id) = match &iss.target {
                            ValidationTarget::Project => ("project", "", ""),
                            ValidationTarget::Variable { variable_id } => {
                                ("variable", "", variable_id.as_str())
                            }
                            ValidationTarget::Record { record_id } => {
                                ("record", record_id.as_str(), "")
                            }
                            ValidationTarget::Cell {
                                record_id,
                                variable_id,
                            } => ("cell", record_id.as_str(), variable_id.as_str()),
                            ValidationTarget::Asset { asset_id } => {
                                ("asset", asset_id.as_str(), "")
                            }
                            ValidationTarget::ExportTarget { .. } => ("export", "", ""),
                        };
                        AuditIssueItem {
                            code: iss.code.code_str().into(),
                            severity_kind: if iss.severity == ValidationSeverity::BlockingError {
                                0
                            } else {
                                1
                            },
                            title: iss.title.as_str().into(),
                            message: iss.message.as_str().into(),
                            target_kind: target_kind.into(),
                            target_record_id: rec_id.into(),
                            target_var_id: var_id.into(),
                        }
                    })
                    .collect();
                app.set_audit_issues(ModelRc::new(Rc::new(VecModel::from(issues))));
            }
            Err(err) => self.show_error(app, err),
        }
    }

    fn navigate_to_audit_issue(
        &mut self,
        app: &AppWindow,
        target_kind: &str,
        rec_id: &str,
        var_id: &str,
    ) {
        let Some(ref pid) = self.active_project_id.clone() else {
            return;
        };
        let Ok(snap) = self.services.project_service.open_project(pid) else {
            return;
        };

        if let Some(var) = snap.variable_by_id(var_id)
            && var.kind == VariableKind::LinkedFile
            && target_kind == "cell"
        {
            if let Some(pidx) = snap
                .variables
                .iter()
                .filter(|v| v.kind == VariableKind::LinkedFile)
                .position(|v| v.id == var.id)
            {
                self.photo_state.selected_photo_col_index = pidx;
            }
            self.photo_state.filter_index = 0;
            self.photo_state.selected_cell_index =
                snap.records.iter().position(|r| r.id == rec_id);
            app.set_photo_filter_index(0);
            app.set_active_tab(2);
            self.refresh_photos_with_snap(app, &snap, true);
            self.update_tab_header(app);
            return;
        }

        if target_kind == "variable" {
            app.set_show_columns_panel(true);
            app.set_active_tab(1);
            self.refresh_grid_with_snap(app, &snap);
            self.update_tab_header(app);
            return;
        }

        if !rec_id.is_empty()
            && let Some(ridx) = snap.records.iter().position(|r| r.id == rec_id)
        {
            self.grid_state.current_page = (ridx / GRID_PAGE_SIZE) + 1;
            self.grid_state.selected_record_ids.clear();
            self.grid_state.selected_record_ids.insert(rec_id.to_string());
            app.set_active_tab(1);
            self.refresh_grid_with_snap(app, &snap);
            self.update_tab_header(app);
        }
    }

    fn run_export(
        &mut self,
        app: &AppWindow,
        project_id: &str,
        target_dir: PathBuf,
        overwrite_confirmed: bool,
    ) {
        app.set_is_busy(true);
        let result = self
            .services
            .export_service
            .export_package(project_id, &target_dir, overwrite_confirmed);
        app.set_is_busy(false);

        match result {
            Ok(summary) => {
                let msg = format!(
                    "Exportado para '{}' — {} Data Sets, {} foto(s) ({} KB).",
                    summary.target_dir.display(),
                    summary.exported_datasets,
                    summary.exported_assets,
                    summary.total_asset_bytes / 1024
                );
                app.set_last_export_summary(msg.as_str().into());
                self.refresh_xml_export_tab(app);
                self.show_banner(app, 1, &msg);
            }
            Err(AppError::Domain(DomainError::OverwriteConfirmationRequired {
                target_dir: dir_str,
                existing_paths,
            })) => {
                let list_str = existing_paths.join("\n• ");
                self.open_confirm_dialog(
                    app,
                    "Sobrescrever Exportação Existente?",
                    &format!(
                        "A pasta '{dir_str}' já contém arquivos de pacote anteriores:\n• {list_str}\n\nDeseja substituí-los de forma atômica?"
                    ),
                    "Sobrescrever Pacote",
                    true,
                    PendingConfirm::OverwriteExport {
                        project_id: project_id.to_string(),
                        target_dir,
                    },
                );
            }
            Err(err) => self.show_error(app, err),
        }
    }

    fn scan_photo_folder_internal(&mut self, app: &AppWindow, project_id: &str, folder: &std::path::Path) {
        app.set_is_busy(true);
        let res = self.services.photo_service.scan_directory(project_id, folder);
        app.set_is_busy(false);
        match res {
            Ok(scan) => {
                self.refresh_all_active_project(app);
                self.show_banner(
                    app,
                    1,
                    &format!(
                        "Varredura concluída: {} imagem(ns) suportada(s), {} arquivo(s) ignorado(s).",
                        scan.assets.len(),
                        scan.skipped_unsupported
                    ),
                );
            }
            Err(err) => self.show_error(app, err),
        }
    }

    fn batch_link_action(&mut self, app: &AppWindow, multi_mode: bool) {
        let Some(pid) = self.active_project_id.clone() else {
            return;
        };
        let Some(asset_idx) = self.photo_state.selected_pool_asset_index else {
            return;
        };
        let Some(asset_item) = app.get_unmatched_photo_assets().row_data(asset_idx) else {
            return;
        };

        let overwrite = app.get_batch_overwrite_existing();
        let mut pairs = Vec::new();
        let cell_model = app.get_photo_cells();
        if multi_mode {
            for idx in 0..cell_model.row_count() {
                if let Some(cell) = cell_model.row_data(idx)
                    && self.photo_state.batch_selected_record_ids.contains(cell.record_id.as_str())
                {
                    pairs.push((
                        cell.record_id.to_string(),
                        cell.variable_id.to_string(),
                        asset_item.asset_id.to_string(),
                    ));
                }
            }
        } else if let Some(cell_idx) = self.photo_state.selected_cell_index
            && let Some(cell) = cell_model.row_data(cell_idx)
        {
            pairs.push((
                cell.record_id.to_string(),
                cell.variable_id.to_string(),
                asset_item.asset_id.to_string(),
            ));
        }

        if pairs.is_empty() {
            self.show_banner(
                app,
                2,
                "Selecione ao menos um registro (ou marque a caixa de lote) antes de vincular.",
            );
            return;
        }

        match self.services.photo_service.batch_link(&pid, &pairs, overwrite) {
            Ok(res) => {
                self.photo_state.batch_selected_record_ids.clear();
                self.refresh_all_active_project(app);
                let mut msg = format!("{} registro(s) vinculado(s) com sucesso.", res.linked_count);
                if !res.duplicate_asset_warnings.is_empty() {
                    msg.push_str(&format!(
                        " Atenção: {} foto(s) compartilhada(s) em múltiplos registros.",
                        res.duplicate_asset_warnings.len()
                    ));
                }
                self.show_banner(app, 1, &msg);
            }
            Err(err) => self.show_error(app, err),
        }
    }

    fn apply_paste_block(
        &mut self,
        app: &AppWindow,
        row_1based: i32,
        col_idx: i32,
        text: &str,
    ) {
        let Some(pid) = self.active_project_id.clone() else {
            return;
        };
        let start_row = row_1based.max(1) as usize - 1;
        let start_col = col_idx.max(0) as usize;
        match self
            .services
            .data_service
            .paste_block(&pid, start_row, start_col, text)
        {
            Ok(count) => {
                app.set_show_paste_panel(false);
                self.refresh_all_active_project(app);
                self.show_banner(app, 1, &format!("{count} linha(s) colada(s) na tabela."));
            }
            Err(err) => self.show_error(app, err),
        }
    }

    fn nudge_record(&mut self, app: &AppWindow, record_id: &str, delta: isize) {
        let Some(pid) = self.active_project_id.clone() else {
            return;
        };
        let Ok(snap) = self.services.project_service.open_project(&pid) else {
            return;
        };
        let mut ids: Vec<String> = snap.records.iter().map(|r| r.id.clone()).collect();
        let Some(pos) = ids.iter().position(|id| id == record_id) else {
            return;
        };
        let new_pos = pos as isize + delta;
        if new_pos < 0 || new_pos >= ids.len() as isize {
            return;
        }
        ids.swap(pos, new_pos as usize);
        match self.services.data_service.reorder_records(&pid, &ids) {
            Ok(()) => self.refresh_all_active_project(app),
            Err(err) => self.show_error(app, err),
        }
    }

    fn nudge_variable(&mut self, app: &AppWindow, var_id: &str, delta: i32) {
        let Some(pid) = self.active_project_id.clone() else {
            return;
        };
        let Ok(snap) = self.services.project_service.open_project(&pid) else {
            return;
        };
        let mut ids: Vec<String> = snap.variables.iter().map(|v| v.id.clone()).collect();
        let Some(pos) = ids.iter().position(|id| id == var_id) else {
            return;
        };
        let new_pos = pos as isize + delta as isize;
        if new_pos < 0 || new_pos >= ids.len() as isize {
            return;
        }
        ids.swap(pos, new_pos as usize);
        match self.services.project_service.reorder_variables(&pid, &ids) {
            Ok(()) => self.refresh_all_active_project(app),
            Err(err) => self.show_error(app, err),
        }
    }

    fn start_import_wizard(&mut self, app: &AppWindow, path: PathBuf) {
        match self.services.import_service.list_sheets(&path) {
            Ok(sheets) => {
                self.grid_state.import.open = true;
                self.grid_state.import.file_path = Some(path.clone());
                self.grid_state.import.sheets = sheets.clone();
                self.grid_state.import.selected_sheet_index = 0;
                self.grid_state.import.header_row_1based = 1;
                app.set_import_file_path(path.to_string_lossy().as_ref().into());
                app.set_import_sheets(ModelRc::new(Rc::new(VecModel::from(
                    sheets.iter().map(|s| SharedString::from(s.as_str())).collect::<Vec<_>>(),
                ))));
                app.set_import_sheet_index(0);
                app.set_import_header_row_1based(1);
                app.set_show_import_wizard(true);
                self.reload_import_preview(app);
            }
            Err(err) => self.show_error(app, err),
        }
    }

    fn reload_import_preview(&mut self, app: &AppWindow) {
        let Some(ref path) = self.grid_state.import.file_path else {
            return;
        };
        let sheet_name = self
            .grid_state
            .import
            .sheets
            .get(self.grid_state.import.selected_sheet_index)
            .map(String::as_str);
        match self
            .services
            .import_service
            .preview_spreadsheet(path, sheet_name, Some(25))
        {
            Ok(table) => {
                self.grid_state.import.raw_table = Some(table);
                self.rebuild_import_columns_state(app);
            }
            Err(err) => self.show_error(app, err),
        }
    }

    fn rebuild_import_columns_state(&mut self, app: &AppWindow) {
        let Some(ref table) = self.grid_state.import.raw_table else {
            return;
        };
        let existing_vars = self
            .active_project_id
            .as_deref()
            .and_then(|pid| self.services.project_service.open_project(pid).ok())
            .map(|s| s.variables)
            .unwrap_or_default();

        let (cols, previews) = rebuild_import_columns(
            table,
            self.grid_state.import.header_row_1based,
            &existing_vars,
        );
        self.grid_state.import.columns = cols;
        app.set_import_columns(build_import_column_models(&self.grid_state.import.columns));
        app.set_import_preview_rows(ModelRc::new(Rc::new(VecModel::from(previews))));
    }

    fn commit_import_wizard(&mut self, app: &AppWindow) {
        let Some(pid) = self.active_project_id.clone() else {
            return;
        };
        let Some(path) = self.grid_state.import.file_path.clone() else {
            return;
        };
        let sheet = self
            .grid_state
            .import
            .sheets
            .get(self.grid_state.import.selected_sheet_index)
            .cloned();
        let existing_vars = self
            .services
            .project_service
            .open_project(&pid)
            .map(|s| s.variables)
            .unwrap_or_default();

        let mappings = compile_import_mappings(&self.grid_state.import.columns, &existing_vars);
        let mode = if app.get_import_mode_index() == 1 {
            ImportMode::Replace
        } else {
            ImportMode::Append
        };
        let header_row_0based = self.grid_state.import.header_row_1based.saturating_sub(1);

        match self.services.import_service.commit_import(
            &pid,
            &path,
            sheet.as_deref(),
            header_row_0based,
            &mappings,
            mode,
        ) {
            Ok(imported) => {
                self.grid_state.import.open = false;
                app.set_show_import_wizard(false);
                self.grid_state.current_page = 1;
                self.refresh_all_active_project(app);
                self.refresh_projects_list(app);
                self.show_banner(
                    app,
                    1,
                    &format!("{imported} registro(s) importado(s) com sucesso (arquivo original intacto)."),
                );
            }
            Err(err) => self.show_error(app, err),
        }
    }

    fn current_total_pages(&self, project_id: &str) -> usize {
        self.services
            .project_service
            .open_project(project_id)
            .map(|s| s.records.len().div_ceil(GRID_PAGE_SIZE).max(1))
            .unwrap_or(1)
    }

    fn refresh_settings_metrics(&self, app: &AppWindow) {
        let count = self
            .services
            .photo_service
            .thumbnail_cache_len()
            .unwrap_or(0);
        app.set_thumbnail_cache_count(count as i32);
    }

    fn update_tab_header(&self, app: &AppWindow) {
        let (title, subtitle) = match app.get_active_tab() {
            0 => ("Projetos", "Crie, abra, renomeie ou gerencie projetos locais do Illustrator"),
            1 => ("Dados e Colunas", "Edite registros em grade, cole blocos e importe planilhas .xlsx ou .csv"),
            2 => ("Fotos e Vínculos", "Varredura somente leitura, match automático por nome e vínculo manual/em lote"),
            3 => ("XML e Exportação", "Auditoria pré-exportação, XML-base do Illustrator e pacote portátil atômico"),
            _ => ("Configurações", "Tema visual, cache de miniaturas e resumo operacional offline"),
        };
        app.set_view_title(title.into());
        app.set_view_subtitle(subtitle.into());
        app.set_status_text(if self.active_project_id.is_some() {
            format!(
                "Projeto ativo: {}  •  {} registro(s)",
                app.get_active_project_name(),
                app.get_total_records()
            )
            .into()
        } else {
            "Nenhum projeto aberto — selecione ou crie um projeto para começar.".into()
        });
    }

    fn open_confirm_dialog(
        &mut self,
        app: &AppWindow,
        title: &str,
        message: &str,
        confirm_label: &str,
        destructive: bool,
        action: PendingConfirm,
    ) {
        self.pending_confirm = Some(action);
        app.set_confirm_title(title.into());
        app.set_confirm_message(message.into());
        app.set_confirm_button_label(confirm_label.into());
        app.set_confirm_destructive(destructive);
        app.set_confirm_open(true);
    }

    fn show_error(&mut self, app: &AppWindow, err: AppError) {
        self.show_banner(app, 3, &err.user_message());
    }

    fn show_banner(&mut self, app: &AppWindow, kind: i32, text: &str) {
        app.set_banner_kind(kind);
        app.set_banner_text(text.into());
    }

    fn clear_banner(&mut self, app: &AppWindow) {
        app.set_banner_kind(0);
        app.set_banner_text("".into());
    }
}

fn index_to_theme(idx: i32) -> ThemePreference {
    match idx {
        1 => ThemePreference::Light,
        2 => ThemePreference::Dark,
        _ => ThemePreference::System,
    }
}

fn apply_theme(app: &AppWindow, pref: ThemePreference) {
    let (scheme, idx) = match pref {
        ThemePreference::System => (ColorScheme::Unknown, 0),
        ThemePreference::Light => (ColorScheme::Light, 1),
        ThemePreference::Dark => (ColorScheme::Dark, 2),
    };
    Palette::get(app).set_color_scheme(scheme);
    ThemeTokens::get(app).set_theme_mode_index(idx);
    app.set_theme_mode_index(idx);
}
