use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use ai_variable_manager::domain::project::ProjectMode;
use ai_variable_manager::domain::variable::VariableKind;
use ai_variable_manager::presentation::{AppController, AppWindow, ColorScheme, Palette, ThemeTokens};
use ai_variable_manager::AppServices;
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, WindowAdapter};
use slint::{Global, Model, PhysicalSize};
use tempfile::tempdir;

struct HeadlessPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for HeadlessPlatform {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
}

fn render_png(window: &MinimalSoftwareWindow, out_path: &std::path::Path) {
    let width = 1240u32;
    let height = 780u32;
    window.set_size(PhysicalSize::new(width, height));
    window.request_redraw();

    let mut buffer = vec![PremultipliedRgbaColor::default(); (width * height) as usize];
    window.draw_if_needed(|renderer| {
        renderer.render(&mut buffer, width as usize);
    });

    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for px in buffer {
        rgba.push(px.red);
        rgba.push(px.green);
        rgba.push(px.blue);
        rgba.push(255);
    }
    let img = image::RgbaImage::from_raw(width, height, rgba).expect("valid RGBA image");
    img.save(out_path).expect("save UI preview PNG");
}

#[test]
fn ui_theme_tokens_and_screens_have_valid_contrast_in_dark_light_and_system_modes() {
    let sw_window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let _ = slint::platform::set_platform(Box::new(HeadlessPlatform {
        window: sw_window.clone(),
    }));

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("ui_test.db");
    let services = Arc::new(AppServices::new_with_db_path(&db_path).unwrap());

    // Create sample project with columns, records and a photo
    let proj = services
        .project_service
        .create_project("Crachás SECOM 2026", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = services
        .project_service
        .create_variable(&proj.id, "Nome", Some("nome"), VariableKind::Text, true)
        .unwrap();
    let v_cargo = services
        .project_service
        .create_variable(&proj.id, "Cargo", Some("cargo"), VariableKind::Text, false)
        .unwrap();
    let v_setor = services
        .project_service
        .create_variable(&proj.id, "Setor", Some("setor"), VariableKind::Text, false)
        .unwrap();
    let v_foto = services
        .project_service
        .create_variable(&proj.id, "Foto", Some("foto"), VariableKind::LinkedFile, false)
        .unwrap();

    let r1 = services.data_service.add_record(&proj.id, Some("Registro_0001")).unwrap();
    services.data_service.update_cell(&proj.id, &r1.id, &v_nome.id, "Ana Souza").unwrap();
    services.data_service.update_cell(&proj.id, &r1.id, &v_cargo.id, "Designer").unwrap();
    services.data_service.update_cell(&proj.id, &r1.id, &v_setor.id, "SECOM").unwrap();
    services.data_service.update_cell(&proj.id, &r1.id, &v_foto.id, "ana_souza").unwrap();

    let r2 = services.data_service.add_record(&proj.id, Some("Registro_0002")).unwrap();
    services.data_service.update_cell(&proj.id, &r2.id, &v_nome.id, "Carlos Lima").unwrap();
    services.data_service.update_cell(&proj.id, &r2.id, &v_cargo.id, "Fotógrafo").unwrap();

    let app = AppWindow::new().unwrap();
    AppController::bootstrap(&app, Arc::clone(&services), db_path);

    // Verify PhotosView exposes all other record columns beneath each record name
    app.invoke_tab_selected(2);
    app.invoke_select_photo_cell(0);
    let cells = app.get_photo_cells();
    assert_eq!(cells.row_count(), 2);
    let c0 = cells.row_data(0).unwrap();
    assert_eq!(
        c0.record_columns_text.as_str(),
        "Nome: Ana Souza  •  Cargo: Designer  •  Setor: SECOM"
    );
    let c1 = cells.row_data(1).unwrap();
    assert_eq!(
        c1.record_columns_text.as_str(),
        "Nome: Carlos Lima  •  Cargo: Fotógrafo  •  Setor: —"
    );

    // 1. Explicit Dark mode contrast check
    Palette::get(&app).set_color_scheme(ColorScheme::Dark);
    let tokens = ThemeTokens::get(&app);
    assert!(tokens.get_is_dark(), "Modo escuro deve ativar ThemeTokens.is_dark");
    assert!(tokens.get_card_bg().red() < 60, "Card em modo escuro deve ser escuro");
    assert!(tokens.get_text_primary().red() > 220, "Texto primário em modo escuro deve ser claro");
    assert!(tokens.get_text_secondary().red() > 150, "Texto secundário em modo escuro deve ser legível");

    // 2. Explicit Light mode contrast check
    Palette::get(&app).set_color_scheme(ColorScheme::Light);
    assert!(!tokens.get_is_dark(), "Modo claro deve desativar ThemeTokens.is_dark");
    assert!(tokens.get_card_bg().red() > 240, "Card em modo claro deve ser claro");
    assert!(tokens.get_text_primary().red() < 50, "Texto primário em modo claro deve ser escuro");
    assert!(tokens.get_text_secondary().red() < 120, "Texto secundário em modo claro deve ser escuro");

    // 3. System / Unknown mode: tokens must NEVER contradict the actual rendered palette background
    Palette::get(&app).set_color_scheme(ColorScheme::Unknown);
    let bg_color = Palette::get(&app).get_background().color();
    let palette_is_dark = bg_color.red() < 128;
    assert_eq!(
        tokens.get_is_dark(),
        palette_is_dark,
        "Modo Sistema deve manter ThemeTokens sincronizado com o fundo real da Fluent Palette"
    );

    // Optional screenshot dump for visual inspection
    if let Ok(dump_dir) = std::env::var("AIVM_DUMP_PREVIEWS_DIR") {
        let out_dir = PathBuf::from(dump_dir);
        std::fs::create_dir_all(&out_dir).unwrap();
        let captured = RefCell::new(Vec::new());

        for (scheme_name, scheme) in [("dark", ColorScheme::Dark), ("light", ColorScheme::Light)] {
            Palette::get(&app).set_color_scheme(scheme);
            for (tab_idx, tab_name) in [
                (0, "projects"),
                (1, "data_grid"),
                (2, "photos"),
                (3, "xml_export"),
                (4, "settings"),
            ] {
                app.invoke_tab_selected(tab_idx);
                let file = out_dir.join(format!("{scheme_name}_{tab_name}.png"));
                render_png(&sw_window, &file);
                captured.borrow_mut().push(file);
            }
        }
        assert_eq!(captured.borrow().len(), 10);
    }
}
