use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ai_variable_manager::domain::{
    REASON_AMBIGUOUS_TIE, REASON_FUZZY_STEM, Asset, ImageFormat, ImportColumnMapping,
    ImportColumnTarget, ImportMode, MatchCandidate, MatchStatus, NewVariableSpec, PathStrategy,
    ProjectMode, Record, TemplateVariableTrait, ThemePreference, ThumbnailState, ValidationCode,
    ValidationTarget, Variable, VariableKind,
};
use ai_variable_manager::shared::error::{AppError, DomainError};
use ai_variable_manager::shared::text_norm::is_valid_xml_ncname;

use super::helpers::{
    MINIMAL_JPEG_2X2, MINIMAL_PNG_2X2, TestEnv, assert_file_unchanged,
    assert_illustrator_xml_matches_columns, assert_illustrator_xml_valid, assert_utf8_no_bom_crlf,
    extract_xml_datasets, fixture_path, generate_non_square_png, generate_synthetic_photo_dir,
    read_fixture_string, read_manifest_json, read_relatorio_csv, request_thumbnail_blocking,
    seed_records_and_values, snapshot_bytes,
};

fn new_var_spec(display_name: &str, kind: VariableKind, required: bool) -> NewVariableSpec {
    NewVariableSpec {
        display_name: display_name.to_string(),
        xml_name: None,
        kind,
        required,
        mapped_template_var: None,
    }
}

fn setup_photo_project(
    env: &TestEnv,
    name: &str,
    lookup_names: &[&str],
) -> (String, Variable, Variable, Vec<Record>) {
    let proj = env
        .services
        .project_service
        .create_project(name, ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&proj.id, "Foto", Some("Foto"), VariableKind::LinkedFile, false)
        .unwrap();

    let rows: Vec<(String, Vec<(&Variable, &str)>)> = lookup_names
        .iter()
        .enumerate()
        .map(|(i, val)| {
            (
                format!("DS_{:03}", i + 1),
                vec![(&v_nome, *val), (&v_foto, *val)],
            )
        })
        .collect();
    let records = seed_records_and_values(&env.services, &proj.id, &rows);
    (proj.id, v_nome, v_foto, records)
}

// ============================================================================
// F-03: SQLite Persistence & Migrations (5 tests)
// ============================================================================

#[test]
fn t1_f03_01_fresh_db_initializes_schema() {
    let env = TestEnv::new();
    assert!(env.db_path.exists());
    let projects = env.services.project_service.list_projects().unwrap();
    assert!(projects.is_empty());
}

#[test]
fn t1_f03_02_reopen_preserves_full_project_snapshot() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Crachás SECOM", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&proj.id, "Foto", Some("Foto"), VariableKind::LinkedFile, false)
        .unwrap();

    let seeded = seed_records_and_values(
        &env.services,
        &proj.id,
        &[
            ("01 - Ana".into(), vec![(&v_nome, "Ana Paula"), (&v_foto, "ana.jpg")]),
            ("02 - Bruno".into(), vec![(&v_nome, "Bruno Lima"), (&v_foto, "bruno.png")]),
            ("03 - Carla".into(), vec![(&v_nome, "Carla Dias"), (&v_foto, "")]),
        ],
    );

    let reopened = env.reopen();
    let snap = reopened.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.project.name, "Crachás SECOM");
    assert_eq!(snap.variables.len(), 2);
    assert_eq!(snap.variables[0].kind, VariableKind::Text);
    assert_eq!(snap.variables[1].kind, VariableKind::LinkedFile);
    assert_eq!(snap.records.len(), 3);
    assert_eq!(
        snap.value_for(&seeded[0].id, &v_nome.id).unwrap().text_value,
        "Ana Paula"
    );
    assert_eq!(
        snap.value_for(&seeded[1].id, &v_foto.id).unwrap().text_value,
        "bruno.png"
    );
    assert_eq!(
        snap.value_for(&seeded[2].id, &v_nome.id).unwrap().text_value,
        "Carla Dias"
    );
}

#[test]
fn t1_f03_03_delete_project_cascades_children() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Projeto Cascata", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&proj.id, "Foto", Some("Foto"), VariableKind::LinkedFile, true)
        .unwrap();

    let asset = Asset {
        id: "asset-1".into(),
        project_id: proj.id.clone(),
        source_path: env.path().join("ana.jpg"),
        file_name: "ana.jpg".into(),
        stem: "ana".into(),
        normalized_stem: "ana".into(),
        format: ImageFormat::Jpg,
        file_size_bytes: 683,
        modified_epoch_ms: 1_700_000_000_000,
        exists_on_disk: true,
    };
    env.services
        .project_repo
        .replace_assets(&proj.id, &[asset])
        .unwrap();

    let records = seed_records_and_values(
        &env.services,
        &proj.id,
        &[("DS_01".into(), vec![(&v_foto, "ana")])],
    );
    env.services
        .project_repo
        .update_match_candidates(
            &proj.id,
            &[MatchCandidate {
                record_id: records[0].id.clone(),
                variable_id: v_foto.id.clone(),
                asset_id: "asset-1".into(),
                score: 0.95,
                reason: REASON_FUZZY_STEM.into(),
            }],
        )
        .unwrap();

    env.services.project_service.delete_project(&proj.id).unwrap();
    assert!(env.services.project_service.list_projects().unwrap().is_empty());

    let err = env.services.project_service.open_project(&proj.id).unwrap_err();
    assert!(matches!(err, AppError::Domain(DomainError::NotFound { .. })));
}

#[test]
fn t1_f03_04_wal_concurrent_read_consistency() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("WAL Test", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_cod = env
        .services
        .project_service
        .create_variable(&proj.id, "Codigo", Some("Codigo"), VariableKind::Text, true)
        .unwrap();

    let reader = env.reopen();
    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[("DS_001".into(), vec![(&v_cod, "00421")])],
    );

    let snap = reader.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.records.len(), 1);
    assert_eq!(
        snap.value_for(&recs[0].id, &v_cod.id).unwrap().text_value,
        "00421"
    );
}

#[test]
fn t1_f03_05_idempotent_reinitialization_on_existing_db() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Persistente", ProjectMode::WithoutTemplate)
        .unwrap();
    let var = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();

    let s2 = env.reopen();
    let s3 = s2
        .project_service
        .open_project(&proj.id)
        .expect("second handle reads cleanly");
    assert_eq!(s3.variables.len(), 1);
    assert_eq!(s3.variables[0].id, var.id);

    let third = env.reopen();
    let snap = third.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.project.name, "Persistente");
    assert_eq!(snap.variables.len(), 1);
    assert_eq!(snap.variables[0].id, var.id);
}

// ============================================================================
// F-04: Project Lifecycle Management (5 tests)
// ============================================================================

#[test]
fn t1_f04_01_create_project_with_template_mode() {
    let env = TestEnv::new();
    let p = env
        .services
        .project_service
        .create_project("Crachás SECOM", ProjectMode::WithTemplate)
        .unwrap();
    let parsed_uuid = uuid::Uuid::parse_str(&p.id).expect("valid UUID");
    assert_eq!(parsed_uuid.get_version(), Some(uuid::Version::Random));
    assert_eq!(p.name, "Crachás SECOM");
    assert_eq!(p.mode, ProjectMode::WithTemplate);
    assert_eq!(p.path_strategy, PathStrategy::RelativeToXml);
}

#[test]
fn t1_f04_02_create_project_without_template_mode() {
    let env = TestEnv::new();
    let p = env
        .services
        .project_service
        .create_project("Cadastro Livre", ProjectMode::WithoutTemplate)
        .unwrap();
    assert_eq!(p.mode, ProjectMode::WithoutTemplate);
    assert!(p.xml_template_raw.is_none());
    assert!(p.xml_template_path.is_none());
}

#[test]
fn t1_f04_03_list_projects_ordered_by_recent() {
    let env = TestEnv::new();
    let p1 = env
        .services
        .project_service
        .create_project("P1", ProjectMode::WithoutTemplate)
        .unwrap();
    std::thread::sleep(Duration::from_millis(8));
    let p2 = env
        .services
        .project_service
        .create_project("P2", ProjectMode::WithoutTemplate)
        .unwrap();
    std::thread::sleep(Duration::from_millis(8));
    let p3 = env
        .services
        .project_service
        .create_project("P3", ProjectMode::WithoutTemplate)
        .unwrap();
    std::thread::sleep(Duration::from_millis(8));

    env.services
        .project_service
        .rename_project(&p1.id, "P1 Atualizado")
        .unwrap();

    let list = env.services.project_service.list_projects().unwrap();
    assert_eq!(list.len(), 3);
    assert_eq!(list[0].id, p1.id);
    assert_eq!(list[1].id, p3.id);
    assert_eq!(list[2].id, p2.id);
    assert!(list[0].updated_at > list[1].updated_at);
}

#[test]
fn t1_f04_04_rename_and_update_project_settings() {
    let env = TestEnv::new();
    let p = env
        .services
        .project_service
        .create_project("SECOM Rascunho", ProjectMode::WithTemplate)
        .unwrap();
    env.services
        .project_service
        .rename_project(&p.id, "SECOM 2026")
        .unwrap();
    let mut snap = env.services.project_service.open_project(&p.id).unwrap();
    snap.project.path_strategy = PathStrategy::AbsoluteFileUri;
    env.services
        .project_service
        .update_project_settings(&snap.project)
        .unwrap();

    let reopened = env.reopen();
    let after = reopened.project_service.open_project(&p.id).unwrap();
    assert_eq!(after.project.name, "SECOM 2026");
    assert_eq!(after.project.path_strategy, PathStrategy::AbsoluteFileUri);
}

#[test]
fn t1_f04_05_app_settings_theme_persistence() {
    let env = TestEnv::new();
    env.services
        .project_service
        .set_setting("theme", "dark")
        .unwrap();
    env.services
        .project_service
        .set_theme_preference(ThemePreference::Dark)
        .unwrap();

    let r1 = env.reopen();
    assert_eq!(
        r1.project_service.get_setting("theme").unwrap(),
        Some("dark".to_string())
    );
    assert_eq!(
        r1.project_service.get_theme_preference().unwrap(),
        ThemePreference::Dark
    );

    r1.project_service.set_setting("theme", "light").unwrap();
    r1.project_service
        .set_theme_preference(ThemePreference::Light)
        .unwrap();
    let r2 = env.reopen();
    assert_eq!(
        r2.project_service.get_setting("theme").unwrap(),
        Some("light".to_string())
    );
    assert_eq!(
        r2.project_service.get_theme_preference().unwrap(),
        ThemePreference::Light
    );

    r2.project_service.set_setting("theme", "system").unwrap();
    r2.project_service
        .set_theme_preference(ThemePreference::System)
        .unwrap();
    let r3 = env.reopen();
    assert_eq!(
        r3.project_service.get_setting("theme").unwrap(),
        Some("system".to_string())
    );
    assert_eq!(
        r3.project_service.get_theme_preference().unwrap(),
        ThemePreference::System
    );
}

// ============================================================================
// F-06: Virtualized Tabular Grid Data Scale (5 tests)
// ============================================================================

#[test]
fn t1_f06_01_windowed_slice_fetch_preserves_sort_order() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Grid Window", ProjectMode::WithoutTemplate)
        .unwrap();
    let v = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();

    for i in 0..50 {
        let rec = env
            .services
            .data_service
            .add_record(&proj.id, Some(&format!("DS_{i:02}")))
            .unwrap();
        env.services
            .data_service
            .update_cell(&proj.id, &rec.id, &v.id, &format!("Pessoa {i:02}"))
            .unwrap();
    }

    let win = env
        .services
        .data_service
        .get_records_window(&proj.id, 10, 10)
        .unwrap();
    assert_eq!(win.total_records, 50);
    assert_eq!(win.offset, 10);
    assert_eq!(win.records.len(), 10);
    let orders: Vec<usize> = win.records.iter().map(|r| r.sort_order).collect();
    assert_eq!(orders, (10..20).collect::<Vec<_>>());
    assert_eq!(win.records[0].dataset_name, "DS_10");
}

#[test]
fn t1_f06_02_1000_row_dataset_loads_cleanly() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Mil Registros", ProjectMode::WithoutTemplate)
        .unwrap();
    let mut vars = Vec::new();
    for idx in 1..=5 {
        vars.push(
            env.services
                .project_service
                .create_variable(
                    &proj.id,
                    &format!("Texto {idx}"),
                    Some(&format!("Texto{idx}")),
                    VariableKind::Text,
                    false,
                )
                .unwrap(),
        );
    }
    vars.push(
        env.services
            .project_service
            .create_variable(&proj.id, "Foto", Some("Foto"), VariableKind::LinkedFile, false)
            .unwrap(),
    );

    let mut tsv = String::new();
    for r in 0..1000 {
        tsv.push_str(&format!(
            "V1_{r}\tV2_{r}\tV3_{r}\tV4_{r}\tV5_{r}\tPessoa_{:04}.jpg\n",
            r + 1
        ));
    }
    let pasted = env
        .services
        .data_service
        .paste_block(&proj.id, 0, 0, &tsv)
        .unwrap();
    assert_eq!(pasted, 1000);

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.records.len(), 1000);
    assert_eq!(snap.values.len(), 6000);

    let win = env
        .services
        .data_service
        .get_records_window(&proj.id, 900, 100)
        .unwrap();
    assert_eq!(win.total_records, 1000);
    assert_eq!(win.records.len(), 100);
    assert_eq!(win.records[0].sort_order, 900);
    assert_eq!(win.records[99].sort_order, 999);
}

#[test]
fn t1_f06_03_filter_or_lookup_by_record_order() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Ordem Estável", ProjectMode::WithoutTemplate)
        .unwrap();
    let v = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, false)
        .unwrap();

    let mut recs = Vec::new();
    for i in 0..25 {
        let r = env
            .services
            .data_service
            .add_record(&proj.id, Some(&format!("R_{i:02}")))
            .unwrap();
        env.services
            .data_service
            .update_cell(&proj.id, &r.id, &v.id, &format!("Valor {i:02}"))
            .unwrap();
        recs.push(r);
    }

    env.services
        .data_service
        .update_cell(&proj.id, &recs[12].id, &v.id, "Valor Editado 12")
        .unwrap();

    let win = env
        .services
        .data_service
        .get_records_window(&proj.id, 0, 25)
        .unwrap();
    let orders: Vec<usize> = win.records.iter().map(|r| r.sort_order).collect();
    assert_eq!(orders, (0..25).collect::<Vec<_>>());
    let cell_12 = win
        .values
        .iter()
        .find(|val| val.record_id == recs[12].id && val.variable_id == v.id)
        .unwrap();
    assert_eq!(cell_12.text_value, "Valor Editado 12");
}

#[test]
fn t1_f06_04_sparse_eav_values_default_to_empty() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("EAV Esparso", ProjectMode::WithoutTemplate)
        .unwrap();
    let v1 = env
        .services
        .project_service
        .create_variable(&proj.id, "Col1", Some("Col1"), VariableKind::Text, false)
        .unwrap();
    for i in 0..20 {
        let rec = env
            .services
            .data_service
            .add_record(&proj.id, Some(&format!("DS_{i:02}")))
            .unwrap();
        env.services
            .data_service
            .update_cell(&proj.id, &rec.id, &v1.id, &format!("Item {i}"))
            .unwrap();
    }

    let v2 = env
        .services
        .project_service
        .create_variable(&proj.id, "Col2", Some("Col2"), VariableKind::Text, false)
        .unwrap();

    let win = env
        .services
        .data_service
        .get_records_window(&proj.id, 0, 20)
        .unwrap();
    assert_eq!(win.records.len(), 20);
    for rec in &win.records {
        let text = win
            .values
            .iter()
            .find(|val| val.record_id == rec.id && val.variable_id == v2.id)
            .map(|val| val.text_value.as_str())
            .unwrap_or("");
        assert_eq!(text, "");
    }
    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.variables.len(), 2);
}

#[test]
fn t1_f06_05_reorder_records_updates_sort_order_persistently() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Reordenar Linhas", ProjectMode::WithoutTemplate)
        .unwrap();
    let _v = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, false)
        .unwrap();

    let r1 = env.services.data_service.add_record(&proj.id, Some("R1")).unwrap();
    let r2 = env.services.data_service.add_record(&proj.id, Some("R2")).unwrap();
    let r3 = env.services.data_service.add_record(&proj.id, Some("R3")).unwrap();

    env.services
        .data_service
        .reorder_records(&proj.id, &[r3.id.clone(), r1.id.clone(), r2.id.clone()])
        .unwrap();

    let reopened = env.reopen();
    let win = reopened
        .data_service
        .get_records_window(&proj.id, 0, 10)
        .unwrap();
    let ids: Vec<&str> = win.records.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec![r3.id.as_str(), r1.id.as_str(), r2.id.as_str()]);
    let orders: Vec<usize> = win.records.iter().map(|r| r.sort_order).collect();
    assert_eq!(orders, vec![0, 1, 2]);
}

// ============================================================================
// F-07: Cell Editing & Block Clipboard Paste (5 tests)
// ============================================================================

#[test]
fn t1_f07_01_single_cell_edit_persists_verbatim() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Edição Literal", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, false)
        .unwrap();
    let v_cod = env
        .services
        .project_service
        .create_variable(&proj.id, "Codigo", Some("Codigo"), VariableKind::Text, false)
        .unwrap();

    let r1 = env.services.data_service.add_record(&proj.id, Some("R1")).unwrap();
    let r2 = env.services.data_service.add_record(&proj.id, Some("R2")).unwrap();

    env.services
        .data_service
        .update_cell(&proj.id, &r1.id, &v_nome.id, "  João  Silva ")
        .unwrap();
    env.services
        .data_service
        .update_cell(&proj.id, &r2.id, &v_cod.id, "00421")
        .unwrap();

    let reopened = env.reopen();
    let snap = reopened.project_service.open_project(&proj.id).unwrap();
    assert_eq!(
        snap.value_for(&r1.id, &v_nome.id).unwrap().text_value,
        "  João  Silva "
    );
    assert_eq!(
        snap.value_for(&r2.id, &v_cod.id).unwrap().text_value,
        "00421"
    );
}

#[test]
fn t1_f07_02_add_and_delete_multiple_rows() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("CRUD Linhas", ProjectMode::WithoutTemplate)
        .unwrap();
    let v = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, false)
        .unwrap();

    let mut recs = Vec::new();
    for i in 1..=5 {
        let r = env
            .services
            .data_service
            .add_record(&proj.id, Some(&format!("R{i}")))
            .unwrap();
        env.services
            .data_service
            .update_cell(&proj.id, &r.id, &v.id, &format!("Pessoa {i}"))
            .unwrap();
        recs.push(r);
    }

    env.services
        .data_service
        .delete_records(&proj.id, &[recs[1].id.clone(), recs[3].id.clone()])
        .unwrap();

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.records.len(), 3);
    assert_eq!(snap.records[0].id, recs[0].id);
    assert_eq!(snap.records[1].id, recs[2].id);
    assert_eq!(snap.records[2].id, recs[4].id);
    assert_eq!(
        snap.records.iter().map(|r| r.sort_order).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(
        snap.value_for(&recs[2].id, &v.id).unwrap().text_value,
        "Pessoa 3"
    );
}

#[test]
fn t1_f07_03_paste_tsv_block_crlf() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Colar TSV", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, false)
        .unwrap();
    let v_setor = env
        .services
        .project_service
        .create_variable(&proj.id, "Setor", Some("Setor"), VariableKind::Text, false)
        .unwrap();

    for i in 0..3 {
        env.services
            .data_service
            .add_record(&proj.id, Some(&format!("DS_{i}")))
            .unwrap();
    }

    let pasted = env
        .services
        .data_service
        .paste_block(&proj.id, 0, 0, "Ana\tTI\r\nBruno\tRH\r\nCarla\tDesign\r\n")
        .unwrap();
    assert_eq!(pasted, 3);

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.records.len(), 3);
    assert_eq!(snap.value_for(&snap.records[0].id, &v_nome.id).unwrap().text_value, "Ana");
    assert_eq!(snap.value_for(&snap.records[0].id, &v_setor.id).unwrap().text_value, "TI");
    assert_eq!(snap.value_for(&snap.records[1].id, &v_nome.id).unwrap().text_value, "Bruno");
    assert_eq!(snap.value_for(&snap.records[1].id, &v_setor.id).unwrap().text_value, "RH");
    assert_eq!(snap.value_for(&snap.records[2].id, &v_nome.id).unwrap().text_value, "Carla");
    assert_eq!(snap.value_for(&snap.records[2].id, &v_setor.id).unwrap().text_value, "Design");
}

#[test]
fn t1_f07_04_paste_csv_quoted_multiline_cell() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Colar CSV", ProjectMode::WithoutTemplate)
        .unwrap();
    let v1 = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, false)
        .unwrap();
    let v2 = env
        .services
        .project_service
        .create_variable(&proj.id, "Obs", Some("Obs"), VariableKind::Text, false)
        .unwrap();
    let r = env.services.data_service.add_record(&proj.id, Some("R1")).unwrap();

    let count = env
        .services
        .data_service
        .paste_block(&proj.id, 0, 0, "\"Silva, João\",\"Linha 1\nLinha 2\"\r\n")
        .unwrap();
    assert_eq!(count, 1);

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(
        snap.value_for(&r.id, &v1.id).unwrap().text_value,
        "Silva, João"
    );
    assert_eq!(
        snap.value_for(&r.id, &v2.id).unwrap().text_value,
        "Linha 1\nLinha 2"
    );
}

#[test]
fn t1_f07_05_paste_expands_rows_when_exceeding_grid_len() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Expansão Colagem", ProjectMode::WithoutTemplate)
        .unwrap();
    let v1 = env
        .services
        .project_service
        .create_variable(&proj.id, "Letra", Some("Letra"), VariableKind::Text, false)
        .unwrap();
    let v2 = env
        .services
        .project_service
        .create_variable(&proj.id, "Num", Some("Num"), VariableKind::Text, false)
        .unwrap();
    env.services.data_service.add_record(&proj.id, Some("R0")).unwrap();

    let pasted = env
        .services
        .data_service
        .paste_block(&proj.id, 0, 0, "A\t1\nB\t2\nC\t3\nD\t4\nE\t5\n")
        .unwrap();
    assert_eq!(pasted, 5);

    let reopened = env.reopen();
    let snap = reopened.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.records.len(), 5);
    assert_eq!(snap.value_for(&snap.records[4].id, &v1.id).unwrap().text_value, "E");
    assert_eq!(snap.value_for(&snap.records[4].id, &v2.id).unwrap().text_value, "5");
}

// ============================================================================
// F-08: Dynamic Column Management (5 tests)
// ============================================================================

#[test]
fn t1_f08_01_create_text_and_linked_file_columns() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Colunas", ProjectMode::WithoutTemplate)
        .unwrap();
    let v1 = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", None, VariableKind::Text, true)
        .unwrap();
    let v2 = env
        .services
        .project_service
        .create_variable(&proj.id, "Foto", None, VariableKind::LinkedFile, false)
        .unwrap();

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.variables.len(), 2);
    assert_eq!(snap.variables[0].id, v1.id);
    assert_eq!(snap.variables[0].sort_order, 0);
    assert_eq!(snap.variables[0].kind, VariableKind::Text);
    assert_eq!(snap.variables[1].id, v2.id);
    assert_eq!(snap.variables[1].sort_order, 1);
    assert_eq!(snap.variables[1].kind, VariableKind::LinkedFile);
}

#[test]
fn t1_f08_02_rename_column_preserves_record_values() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Renomear Coluna", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_cargo = env
        .services
        .project_service
        .create_variable(&proj.id, "Cargo", Some("Cargo"), VariableKind::Text, false)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[
            ("R1".into(), vec![(&v_cargo, "Analista")]),
            ("R2".into(), vec![(&v_cargo, "Técnico")]),
            ("R3".into(), vec![(&v_cargo, "Designer")]),
            ("R4".into(), vec![(&v_cargo, "Coordenador")]),
            ("R5".into(), vec![(&v_cargo, "Assistente")]),
        ],
    );

    let updated = env
        .services
        .project_service
        .rename_variable(&proj.id, &v_cargo.id, "Função", Some("Funcao"))
        .unwrap();
    assert_eq!(updated.id, v_cargo.id);
    assert_eq!(updated.display_name, "Função");
    assert_eq!(updated.xml_name, "Funcao");

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.variable_by_id(&v_cargo.id).unwrap().display_name, "Função");
    assert_eq!(snap.value_for(&recs[0].id, &v_cargo.id).unwrap().text_value, "Analista");
    assert_eq!(snap.value_for(&recs[4].id, &v_cargo.id).unwrap().text_value, "Assistente");
}

#[test]
fn t1_f08_03_duplicate_column_copies_values_and_unique_xml_name() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Duplicar Coluna", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_cid = env
        .services
        .project_service
        .create_variable(&proj.id, "Cidade", Some("Cidade"), VariableKind::Text, false)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[
            ("R1".into(), vec![(&v_cid, "Goiânia")]),
            ("R2".into(), vec![(&v_cid, "Anápolis")]),
            ("R3".into(), vec![(&v_cid, "Aparecida")]),
            ("R4".into(), vec![(&v_cid, "Rio Verde")]),
        ],
    );

    let dup = env
        .services
        .project_service
        .duplicate_variable(&proj.id, &v_cid.id)
        .unwrap();
    assert_ne!(dup.id, v_cid.id);
    assert_ne!(dup.xml_name, v_cid.xml_name);
    assert_eq!(dup.xml_name, "Cidade_copia");
    assert_eq!(dup.display_name, "Cidade (Cópia)");
    assert_eq!(dup.sort_order, v_cid.sort_order + 1);
    assert!(is_valid_xml_ncname(&dup.xml_name));

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    for (rec, expected) in recs
        .iter()
        .zip(["Goiânia", "Anápolis", "Aparecida", "Rio Verde"])
    {
        assert_eq!(snap.value_for(&rec.id, &dup.id).unwrap().text_value, expected);
    }
}

#[test]
fn t1_f08_04_reorder_columns_persists_without_data_loss() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Reordenar Colunas", ProjectMode::WithoutTemplate)
        .unwrap();
    let va = env
        .services
        .project_service
        .create_variable(&proj.id, "A", Some("A"), VariableKind::Text, false)
        .unwrap();
    let vb = env
        .services
        .project_service
        .create_variable(&proj.id, "B", Some("B"), VariableKind::Text, false)
        .unwrap();
    let vc = env
        .services
        .project_service
        .create_variable(&proj.id, "C", Some("C"), VariableKind::Text, false)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[("R1".into(), vec![(&va, "valA"), (&vb, "valB"), (&vc, "valC")])],
    );

    env.services
        .project_service
        .reorder_variables(&proj.id, &[vc.id.clone(), va.id.clone(), vb.id.clone()])
        .unwrap();

    let reopened = env.reopen();
    let snap = reopened.project_service.open_project(&proj.id).unwrap();
    let order: Vec<(&str, usize)> = snap
        .variables
        .iter()
        .map(|v| (v.xml_name.as_str(), v.sort_order))
        .collect();
    assert_eq!(order, vec![("C", 0), ("A", 1), ("B", 2)]);
    assert_eq!(snap.value_for(&recs[0].id, &va.id).unwrap().text_value, "valA");
    assert_eq!(snap.value_for(&recs[0].id, &vb.id).unwrap().text_value, "valB");
    assert_eq!(snap.value_for(&recs[0].id, &vc.id).unwrap().text_value, "valC");
}

#[test]
fn t1_f08_05_delete_column_removes_only_target_values() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Excluir Coluna", ProjectMode::WithoutTemplate)
        .unwrap();
    let va = env
        .services
        .project_service
        .create_variable(&proj.id, "A", Some("A"), VariableKind::Text, false)
        .unwrap();
    let vb = env
        .services
        .project_service
        .create_variable(&proj.id, "B", Some("B"), VariableKind::Text, false)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[
            ("R1".into(), vec![(&va, "A1"), (&vb, "B1")]),
            ("R2".into(), vec![(&va, "A2"), (&vb, "B2")]),
        ],
    );

    env.services
        .project_service
        .delete_variable(&proj.id, &va.id)
        .unwrap();

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.variables.len(), 1);
    assert_eq!(snap.variables[0].id, vb.id);
    assert_eq!(snap.variables[0].sort_order, 0);
    assert!(snap.value_for(&recs[0].id, &va.id).is_none());
    assert_eq!(snap.value_for(&recs[0].id, &vb.id).unwrap().text_value, "B1");
    assert_eq!(snap.value_for(&recs[1].id, &vb.id).unwrap().text_value, "B2");
}

// ============================================================================
// F-09: xml_name & Required Validation (5 tests)
// ============================================================================

#[test]
fn t1_f09_01_valid_ncname_accepted() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("NCName Válido", ProjectMode::WithoutTemplate)
        .unwrap();

    for xml in ["NomeFuncionario", "_col_1", "var-2.pt"] {
        let v = env
            .services
            .project_service
            .create_variable(&proj.id, xml, Some(xml), VariableKind::Text, false)
            .unwrap();
        assert_eq!(v.xml_name, xml);
    }
}

#[test]
fn t1_f09_02_auto_suggest_sanitizes_portuguese_display_name() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Auto Sugestão", ProjectMode::WithoutTemplate)
        .unwrap();
    let v = env
        .services
        .project_service
        .create_variable(&proj.id, "Função do Servidor (1º)", None, VariableKind::Text, false)
        .unwrap();
    assert!(is_valid_xml_ncname(&v.xml_name));
    assert!(!v.xml_name.contains(' '));
    assert!(v.xml_name.is_ascii());
    assert_eq!(v.xml_name, "Funcao_do_Servidor_1");
}

#[test]
fn t1_f09_03_auto_suggest_prefixes_leading_digit() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Dígito Inicial", ProjectMode::WithoutTemplate)
        .unwrap();
    let v = env
        .services
        .project_service
        .create_variable(&proj.id, "1º Turno", None, VariableKind::Text, false)
        .unwrap();
    assert!(is_valid_xml_ncname(&v.xml_name));
    let first = v.xml_name.chars().next().unwrap();
    assert!(first.is_ascii_alphabetic() || first == '_');
    assert_eq!(v.xml_name, "var_1_Turno");
}

#[test]
fn t1_f09_04_toggle_required_flag_persists() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Obrigatoriedade", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let v_obs = env
        .services
        .project_service
        .create_variable(&proj.id, "Obs", Some("Obs"), VariableKind::Text, false)
        .unwrap();

    env.services
        .project_service
        .update_variable_options(&proj.id, &v_nome.id, VariableKind::Text, false, None)
        .unwrap();
    env.services
        .project_service
        .update_variable_options(&proj.id, &v_obs.id, VariableKind::Text, true, None)
        .unwrap();

    let reopened = env.reopen();
    let snap = reopened.project_service.open_project(&proj.id).unwrap();
    assert!(!snap.variable_by_id(&v_nome.id).unwrap().required);
    assert!(snap.variable_by_id(&v_obs.id).unwrap().required);
}

#[test]
fn t1_f09_05_unique_xml_name_across_distinct_projects() {
    let env = TestEnv::new();
    let pa = env
        .services
        .project_service
        .create_project("Projeto A", ProjectMode::WithoutTemplate)
        .unwrap();
    let pb = env
        .services
        .project_service
        .create_project("Projeto B", ProjectMode::WithoutTemplate)
        .unwrap();

    let va = env
        .services
        .project_service
        .create_variable(&pa.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let vb = env
        .services
        .project_service
        .create_variable(&pb.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    assert_eq!(va.xml_name, "Nome");
    assert_eq!(vb.xml_name, "Nome");
}

// ============================================================================
// F-10: .xlsx Importer (5 tests)
// ============================================================================

#[test]
fn t1_f10_01_list_sheets_multi_sheet_workbook() {
    let env = TestEnv::new();
    let sheets = env
        .services
        .import_service
        .list_sheets(&fixture_path("sample_badges.xlsx"))
        .unwrap();
    assert_eq!(sheets, vec!["Crachas".to_string(), "ListaExtra".to_string()]);
}

#[test]
fn t1_f10_02_read_selected_sheet_preview() {
    let env = TestEnv::new();
    let raw = env
        .services
        .import_service
        .preview_spreadsheet(&fixture_path("sample_badges.xlsx"), Some("Crachas"), Some(4))
        .unwrap();
    assert_eq!(raw.rows.len(), 4);
    assert_eq!(
        raw.rows[0],
        vec!["codigo", "nome", "funcao", "numero_raw", "foto"]
    );
    assert!(env.services.project_service.list_projects().unwrap().is_empty());
}

#[test]
fn t1_f10_03_custom_header_row_offset() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("XLSX Header Offset", ProjectMode::WithoutTemplate)
        .unwrap();
    let path = fixture_path("sample_badges.xlsx");
    let preview = env
        .services
        .import_service
        .preview_spreadsheet(&path, Some("ListaExtra"), Some(5))
        .unwrap();
    assert_eq!(
        preview.rows[1],
        vec!["matricula", "servidor", "cargo", "pontuacao"]
    );

    let mappings = vec![
        ImportColumnMapping {
            source_col_index: 0,
            target: ImportColumnTarget::NewVariable(new_var_spec("matricula", VariableKind::Text, true)),
        },
        ImportColumnMapping {
            source_col_index: 1,
            target: ImportColumnTarget::NewVariable(new_var_spec("servidor", VariableKind::Text, true)),
        },
        ImportColumnMapping {
            source_col_index: 2,
            target: ImportColumnTarget::NewVariable(new_var_spec("cargo", VariableKind::Text, false)),
        },
        ImportColumnMapping {
            source_col_index: 3,
            target: ImportColumnTarget::NewVariable(new_var_spec("pontuacao", VariableKind::Text, false)),
        },
    ];

    let imported = env
        .services
        .import_service
        .commit_import(&proj.id, &path, Some("ListaExtra"), 1, &mappings, ImportMode::Append)
        .unwrap();
    assert_eq!(imported, 3);

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.records.len(), 3);
    let v_mat = snap.variables.iter().find(|v| v.xml_name == "matricula").unwrap();
    assert_eq!(snap.value_for(&snap.records[0].id, &v_mat.id).unwrap().text_value, "00888");
}

#[test]
fn t1_f10_04_preserve_text_leading_zeros_from_xlsx() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("XLSX Zeros", ProjectMode::WithoutTemplate)
        .unwrap();
    let path = fixture_path("sample_badges.xlsx");
    let mappings = vec![
        ImportColumnMapping {
            source_col_index: 0,
            target: ImportColumnTarget::NewVariable(new_var_spec("codigo", VariableKind::Text, true)),
        },
        ImportColumnMapping {
            source_col_index: 1,
            target: ImportColumnTarget::NewVariable(new_var_spec("nome", VariableKind::Text, true)),
        },
    ];
    let count = env
        .services
        .import_service
        .commit_import(&proj.id, &path, Some("Crachas"), 0, &mappings, ImportMode::Append)
        .unwrap();
    assert_eq!(count, 5);

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    let v_cod = snap.variables.iter().find(|v| v.xml_name == "codigo").unwrap();
    let codes: Vec<&str> = snap
        .records
        .iter()
        .map(|r| snap.value_for(&r.id, &v_cod.id).unwrap().text_value.as_str())
        .collect();
    assert_eq!(codes, vec!["00421", "00007", "00105", "00421", "00000"]);
}

#[test]
fn t1_f10_05_source_xlsx_file_remains_unmodified() {
    let env = TestEnv::new();
    let orig_path = fixture_path("sample_badges.xlsx");
    let orig_before = snapshot_bytes(&orig_path);
    let temp_copy = env.copy_fixture_file("sample_badges.xlsx", "readonly_copy.xlsx");
    let copy_before = snapshot_bytes(&temp_copy);

    let proj = env
        .services
        .project_service
        .create_project("XLSX RO", ProjectMode::WithoutTemplate)
        .unwrap();
    let _ = env.services.import_service.list_sheets(&temp_copy).unwrap();
    let _ = env
        .services
        .import_service
        .preview_spreadsheet(&temp_copy, Some("Crachas"), Some(10))
        .unwrap();
    let mappings = vec![ImportColumnMapping {
        source_col_index: 0,
        target: ImportColumnTarget::NewVariable(new_var_spec("codigo", VariableKind::Text, true)),
    }];
    env.services
        .import_service
        .commit_import(&proj.id, &temp_copy, Some("Crachas"), 0, &mappings, ImportMode::Append)
        .unwrap();

    assert_file_unchanged(&temp_copy, &copy_before);
    assert_file_unchanged(&orig_path, &orig_before);
}

// ============================================================================
// F-11: .csv Importer (5 tests)
// ============================================================================

#[test]
fn t1_f11_01_autodetect_semicolon_delimiter_utf8() {
    let env = TestEnv::new();
    let path = fixture_path("sample_bombeiros_semicolon.csv");
    let raw = env
        .services
        .import_service
        .preview_spreadsheet(&path, None, Some(10))
        .unwrap();
    assert_eq!(raw.detected_delimiter, Some(';'));
    assert_eq!(raw.rows[0], vec!["matricula", "nome", "funcao", "foto"]);

    let proj = env
        .services
        .project_service
        .create_project("CSV Ponto e Vírgula", ProjectMode::WithoutTemplate)
        .unwrap();
    let mappings = vec![
        ImportColumnMapping {
            source_col_index: 0,
            target: ImportColumnTarget::NewVariable(new_var_spec("matricula", VariableKind::Text, true)),
        },
        ImportColumnMapping {
            source_col_index: 1,
            target: ImportColumnTarget::NewVariable(new_var_spec("nome", VariableKind::Text, true)),
        },
        ImportColumnMapping {
            source_col_index: 2,
            target: ImportColumnTarget::NewVariable(new_var_spec("funcao", VariableKind::Text, false)),
        },
        ImportColumnMapping {
            source_col_index: 3,
            target: ImportColumnTarget::NewVariable(new_var_spec("foto", VariableKind::LinkedFile, false)),
        },
    ];
    let imported = env
        .services
        .import_service
        .commit_import(&proj.id, &path, None, 0, &mappings, ImportMode::Append)
        .unwrap();
    assert_eq!(imported, 6);

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    let v_nome = snap.variables.iter().find(|v| v.xml_name == "nome").unwrap();
    let nomes: Vec<&str> = snap
        .records
        .iter()
        .map(|r| snap.value_for(&r.id, &v_nome.id).unwrap().text_value.as_str())
        .collect();
    assert!(nomes.contains(&"Mariana Vitória Costa"));
    assert!(nomes.contains(&"Bruno César Andrade"));
    assert!(nomes.contains(&"Letícia Moura Santana"));
}

#[test]
fn t1_f11_02_autodetect_comma_and_tab_delimiters() {
    let env = TestEnv::new();
    let comma_raw = env
        .services
        .import_service
        .preview_spreadsheet(&fixture_path("sample_utf8_comma.csv"), None, Some(10))
        .unwrap();
    assert_eq!(comma_raw.detected_delimiter, Some(','));
    assert_eq!(comma_raw.rows[0].len(), 5);

    let tab_raw = env
        .services
        .import_service
        .preview_spreadsheet(&fixture_path("sample_utf8_bom_tab.csv"), None, Some(10))
        .unwrap();
    assert_eq!(tab_raw.detected_delimiter, Some('\t'));
    assert_eq!(tab_raw.rows[0].len(), 4);
}

#[test]
fn t1_f11_03_utf8_bom_stripped_cleanly() {
    let env = TestEnv::new();
    let raw = env
        .services
        .import_service
        .preview_spreadsheet(&fixture_path("sample_utf8_bom_tab.csv"), None, Some(5))
        .unwrap();
    assert_eq!(raw.rows[0][0], "codigo");
    assert!(!raw.rows[0][0].starts_with('\u{FEFF}'));
}

#[test]
fn t1_f11_04_windows_1252_fallback_transcoding() {
    let env = TestEnv::new();
    let path = fixture_path("sample_win1252_semicolon.csv");
    let raw = env
        .services
        .import_service
        .preview_spreadsheet(&path, None, Some(10))
        .unwrap();
    assert_eq!(raw.detected_delimiter, Some(';'));

    let proj = env
        .services
        .project_service
        .create_project("CSV cp1252", ProjectMode::WithoutTemplate)
        .unwrap();
    let mappings = vec![
        ImportColumnMapping {
            source_col_index: 0,
            target: ImportColumnTarget::NewVariable(new_var_spec("matricula", VariableKind::Text, true)),
        },
        ImportColumnMapping {
            source_col_index: 1,
            target: ImportColumnTarget::NewVariable(new_var_spec("nome", VariableKind::Text, true)),
        },
        ImportColumnMapping {
            source_col_index: 2,
            target: ImportColumnTarget::NewVariable(new_var_spec("funcao", VariableKind::Text, false)),
        },
        ImportColumnMapping {
            source_col_index: 3,
            target: ImportColumnTarget::NewVariable(new_var_spec("observacao", VariableKind::Text, false)),
        },
    ];
    let n = env
        .services
        .import_service
        .commit_import(&proj.id, &path, None, 0, &mappings, ImportMode::Append)
        .unwrap();
    assert_eq!(n, 3);

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    let v_nome = snap.variables.iter().find(|v| v.xml_name == "nome").unwrap();
    let v_func = snap.variables.iter().find(|v| v.xml_name == "funcao").unwrap();
    let v_obs = snap.variables.iter().find(|v| v.xml_name == "observacao").unwrap();
    assert_eq!(snap.value_for(&snap.records[0].id, &v_nome.id).unwrap().text_value, "João Carlos");
    assert_eq!(snap.value_for(&snap.records[0].id, &v_func.id).unwrap().text_value, "Designer Gráfico");
    assert_eq!(snap.value_for(&snap.records[0].id, &v_obs.id).unwrap().text_value, "Águia – Seção");
}

#[test]
fn t1_f11_05_quoted_fields_and_header_row_selection() {
    let env = TestEnv::new();
    let comma_path = fixture_path("sample_utf8_comma.csv");
    let header2_path = fixture_path("sample_bombeiros_header_row2.csv");
    let comma_before = snapshot_bytes(&comma_path);
    let header2_before = snapshot_bytes(&header2_path);

    let p1 = env
        .services
        .project_service
        .create_project("CSV Quoted", ProjectMode::WithoutTemplate)
        .unwrap();
    let m1 = vec![
        ImportColumnMapping {
            source_col_index: 0,
            target: ImportColumnTarget::NewVariable(new_var_spec("codigo", VariableKind::Text, true)),
        },
        ImportColumnMapping {
            source_col_index: 1,
            target: ImportColumnTarget::NewVariable(new_var_spec("nome", VariableKind::Text, true)),
        },
        ImportColumnMapping {
            source_col_index: 3,
            target: ImportColumnTarget::NewVariable(new_var_spec("observacoes", VariableKind::Text, false)),
        },
    ];
    let n1 = env
        .services
        .import_service
        .commit_import(&p1.id, &comma_path, None, 0, &m1, ImportMode::Append)
        .unwrap();
    assert_eq!(n1, 6);
    let snap1 = env.services.project_service.open_project(&p1.id).unwrap();
    let v_obs = snap1.variables.iter().find(|v| v.xml_name == "observacoes").unwrap();
    assert_eq!(
        snap1.value_for(&snap1.records[0].id, &v_obs.id).unwrap().text_value,
        "Setor: SECOM, Sala 2"
    );
    assert_eq!(
        snap1.value_for(&snap1.records[1].id, &v_obs.id).unwrap().text_value,
        "Linha 1\nLinha 2"
    );
    assert_eq!(
        snap1.value_for(&snap1.records[2].id, &v_obs.id).unwrap().text_value,
        "Chefe \"Geral\" de Turno"
    );

    let p2 = env
        .services
        .project_service
        .create_project("CSV Header Row 2", ProjectMode::WithoutTemplate)
        .unwrap();
    let m2 = vec![
        ImportColumnMapping {
            source_col_index: 0,
            target: ImportColumnTarget::NewVariable(new_var_spec("matricula", VariableKind::Text, true)),
        },
        ImportColumnMapping {
            source_col_index: 1,
            target: ImportColumnTarget::NewVariable(new_var_spec("nome", VariableKind::Text, true)),
        },
    ];
    let n2 = env
        .services
        .import_service
        .commit_import(&p2.id, &header2_path, None, 1, &m2, ImportMode::Append)
        .unwrap();
    assert_eq!(n2, 4);
    let snap2 = env.services.project_service.open_project(&p2.id).unwrap();
    let v_mat = snap2.variables.iter().find(|v| v.xml_name == "matricula").unwrap();
    assert_eq!(snap2.value_for(&snap2.records[0].id, &v_mat.id).unwrap().text_value, "00012");

    assert_file_unchanged(&comma_path, &comma_before);
    assert_file_unchanged(&header2_path, &header2_before);
}

// ============================================================================
// F-12: Column Mapping & Cell Flags (5 tests)
// ============================================================================

#[test]
fn t1_f12_01_map_to_existing_new_and_ignore() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Mapeamento", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_cod = env
        .services
        .project_service
        .create_variable(&proj.id, "Codigo", Some("Codigo"), VariableKind::Text, true)
        .unwrap();

    let mappings = vec![
        ImportColumnMapping {
            source_col_index: 0,
            target: ImportColumnTarget::ExistingVariable(v_cod.id.clone()),
        },
        ImportColumnMapping {
            source_col_index: 1,
            target: ImportColumnTarget::NewVariable(new_var_spec("Nome", VariableKind::Text, true)),
        },
        ImportColumnMapping {
            source_col_index: 2,
            target: ImportColumnTarget::Ignore,
        },
        ImportColumnMapping {
            source_col_index: 3,
            target: ImportColumnTarget::Ignore,
        },
        ImportColumnMapping {
            source_col_index: 4,
            target: ImportColumnTarget::Ignore,
        },
    ];
    let n = env
        .services
        .import_service
        .commit_import(
            &proj.id,
            &fixture_path("sample_utf8_comma.csv"),
            None,
            0,
            &mappings,
            ImportMode::Append,
        )
        .unwrap();
    assert_eq!(n, 6);

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.variables.len(), 2);
    let names: Vec<&str> = snap.variables.iter().map(|v| v.xml_name.as_str()).collect();
    assert_eq!(names, vec!["Codigo", "Nome"]);
}

#[test]
fn t1_f12_02_preserve_leading_zeros_in_csv_import() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("CSV Zeros", ProjectMode::WithoutTemplate)
        .unwrap();
    let mappings = vec![ImportColumnMapping {
        source_col_index: 0,
        target: ImportColumnTarget::NewVariable(new_var_spec("Codigo", VariableKind::Text, true)),
    }];
    env.services
        .import_service
        .commit_import(
            &proj.id,
            &fixture_path("sample_utf8_comma.csv"),
            None,
            0,
            &mappings,
            ImportMode::Append,
        )
        .unwrap();

    let reopened = env.reopen();
    let snap = reopened.project_service.open_project(&proj.id).unwrap();
    let v_cod = &snap.variables[0];
    let codes: Vec<&str> = snap
        .records
        .iter()
        .map(|r| snap.value_for(&r.id, &v_cod.id).unwrap().text_value.as_str())
        .collect();
    assert_eq!(
        codes,
        vec!["00421", "00007", "00105", "00421", "00999", "00000"]
    );
}

#[test]
fn t1_f12_03_flag_empty_cells_in_import_or_grid() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Células Vazias", ProjectMode::WithoutTemplate)
        .unwrap();
    let mappings = vec![
        ImportColumnMapping {
            source_col_index: 0,
            target: ImportColumnTarget::NewVariable(new_var_spec("matricula", VariableKind::Text, true)),
        },
        ImportColumnMapping {
            source_col_index: 2,
            target: ImportColumnTarget::NewVariable(new_var_spec("funcao", VariableKind::Text, false)),
        },
        ImportColumnMapping {
            source_col_index: 3,
            target: ImportColumnTarget::NewVariable(new_var_spec("foto", VariableKind::LinkedFile, false)),
        },
    ];
    env.services
        .import_service
        .commit_import(
            &proj.id,
            &fixture_path("sample_bombeiros_semicolon.csv"),
            None,
            0,
            &mappings,
            ImportMode::Append,
        )
        .unwrap();

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    let v_func = snap.variables.iter().find(|v| v.xml_name == "funcao").unwrap();
    let v_foto = snap.variables.iter().find(|v| v.xml_name == "foto").unwrap();

    let diags = env
        .services
        .data_service
        .compute_cell_diagnostics(&proj.id)
        .unwrap();

    for empty_row_idx in [0usize, 1, 5] {
        let rid = &snap.records[empty_row_idx].id;
        assert!(
            diags.iter().any(|d| d.record_id == *rid
                && d.variable_id == v_func.id
                && matches!(
                    d.kind,
                    ai_variable_manager::domain::CellDiagnosticKind::EmptyOptional
                        | ai_variable_manager::domain::CellDiagnosticKind::EmptyRequired
                )),
            "expected empty diagnostic for funcao row {empty_row_idx}"
        );
    }
    let last_rid = &snap.records[5].id;
    assert!(diags.iter().any(|d| d.record_id == *last_rid && d.variable_id == v_foto.id));
}

#[test]
fn t1_f12_04_flag_duplicate_values_in_column() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Duplicidades", ProjectMode::WithoutTemplate)
        .unwrap();
    let mappings = vec![
        ImportColumnMapping {
            source_col_index: 0,
            target: ImportColumnTarget::NewVariable(new_var_spec("codigo", VariableKind::Text, true)),
        },
        ImportColumnMapping {
            source_col_index: 1,
            target: ImportColumnTarget::NewVariable(new_var_spec("nome", VariableKind::Text, true)),
        },
    ];
    env.services
        .import_service
        .commit_import(
            &proj.id,
            &fixture_path("sample_utf8_comma.csv"),
            None,
            0,
            &mappings,
            ImportMode::Append,
        )
        .unwrap();

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    let v_cod = snap.variables.iter().find(|v| v.xml_name == "codigo").unwrap();
    let v_nome = snap.variables.iter().find(|v| v.xml_name == "nome").unwrap();
    let diags = env
        .services
        .data_service
        .compute_cell_diagnostics(&proj.id)
        .unwrap();

    for dup_idx in [0usize, 3] {
        let rid = &snap.records[dup_idx].id;
        assert!(diags.iter().any(|d| {
            d.record_id == *rid
                && d.variable_id == v_cod.id
                && d.kind == ai_variable_manager::domain::CellDiagnosticKind::DuplicateValue
        }));
        assert!(diags.iter().any(|d| {
            d.record_id == *rid
                && d.variable_id == v_nome.id
                && d.kind == ai_variable_manager::domain::CellDiagnosticKind::DuplicateValue
        }));
    }
}

#[test]
fn t1_f12_05_reimport_append_or_replace_preserves_existing_columns() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Reimportar", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_cod = env
        .services
        .project_service
        .create_variable(&proj.id, "Codigo", Some("Codigo"), VariableKind::Text, true)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();

    let mappings = vec![
        ImportColumnMapping {
            source_col_index: 0,
            target: ImportColumnTarget::ExistingVariable(v_cod.id.clone()),
        },
        ImportColumnMapping {
            source_col_index: 1,
            target: ImportColumnTarget::ExistingVariable(v_nome.id.clone()),
        },
    ];
    let n1 = env
        .services
        .import_service
        .commit_import(
            &proj.id,
            &fixture_path("sample_utf8_comma.csv"),
            None,
            0,
            &mappings,
            ImportMode::Append,
        )
        .unwrap();
    assert_eq!(n1, 6);

    let n2 = env
        .services
        .import_service
        .commit_import(
            &proj.id,
            &fixture_path("sample_utf8_bom_tab.csv"),
            None,
            0,
            &mappings,
            ImportMode::Append,
        )
        .unwrap();
    assert_eq!(n2, 4);
    let after_append = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(after_append.records.len(), 10);

    let n3 = env
        .services
        .import_service
        .commit_import(
            &proj.id,
            &fixture_path("sample_utf8_bom_tab.csv"),
            None,
            0,
            &mappings,
            ImportMode::Replace,
        )
        .unwrap();
    assert_eq!(n3, 4);
    let after_replace = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(after_replace.records.len(), 4);
    assert_eq!(after_replace.variables.len(), 2);
    assert_eq!(after_replace.variables[0].id, v_cod.id);
    assert_eq!(after_replace.variables[1].id, v_nome.id);
}

// ============================================================================
// F-13: Local Photo Directory Scan (5 tests)
// ============================================================================

#[test]
fn t1_f13_01_scan_all_supported_extensions_case_insensitive() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Scan Extensões", ProjectMode::WithoutTemplate)
        .unwrap();
    let scan = env
        .services
        .photo_service
        .scan_directory(&proj.id, &fixture_path("images"))
        .unwrap();
    assert_eq!(scan.assets.len(), 9);
    let formats: Vec<ImageFormat> = scan.assets.iter().map(|a| a.format).collect();
    assert!(formats.contains(&ImageFormat::Jpg));
    assert!(formats.contains(&ImageFormat::Jpeg));
    assert!(formats.contains(&ImageFormat::Png));
    assert!(formats.contains(&ImageFormat::Tiff));
    assert!(formats.contains(&ImageFormat::Tif));
    assert!(scan.assets.iter().any(|a| a.file_name == "UPPERCASE_EXT.PNG"));
}

#[test]
fn t1_f13_02_recursive_subdirectory_scan() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Scan Recursivo", ProjectMode::WithoutTemplate)
        .unwrap();
    let scan = env
        .services
        .photo_service
        .scan_directory(&proj.id, &fixture_path("images"))
        .unwrap();
    assert!(scan.assets.iter().any(|a| a.file_name == "joao_silva.jpg"));
    let nested = scan
        .assets
        .iter()
        .find(|a| a.file_name == "nested_foto.tif")
        .expect("nested_foto.tif found");
    assert!(nested.source_path.exists());
    assert!(nested.source_path.to_string_lossy().contains("subpasta"));
}

#[test]
fn t1_f13_03_ignore_unsupported_extensions_with_counts() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Scan Ignorados", ProjectMode::WithoutTemplate)
        .unwrap();
    let scan = env
        .services
        .photo_service
        .scan_directory(&proj.id, &fixture_path("images"))
        .unwrap();
    assert_eq!(scan.assets.len(), 9);
    assert_eq!(scan.skipped_unsupported, 2);
    assert_eq!(scan.skipped_by_extension.get("heic").copied(), Some(1));
    assert_eq!(scan.skipped_by_extension.get("pdf").copied(), Some(1));
}

#[test]
fn t1_f13_04_metadata_only_precomputes_normalized_stem() {
    let env = TestEnv::new();
    let img_dir = fixture_path("images");
    let target_file = img_dir.join("MARCELA  GONÇALVES.jpg");
    let before = snapshot_bytes(&target_file);

    let proj = env
        .services
        .project_service
        .create_project("Scan Metadados", ProjectMode::WithoutTemplate)
        .unwrap();
    let _scan = env
        .services
        .photo_service
        .scan_directory(&proj.id, &img_dir)
        .unwrap();

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    let marcela = snap
        .assets
        .iter()
        .find(|a| a.file_name == "MARCELA  GONÇALVES.jpg")
        .unwrap();
    assert_eq!(marcela.file_size_bytes, 683);
    assert!(marcela.modified_epoch_ms > 0);
    assert!(marcela.exists_on_disk);
    assert_eq!(marcela.normalized_stem, "marcela goncalves");
    assert_file_unchanged(&target_file, &before);
}

#[test]
fn t1_f13_05_rescan_updates_assets_idempotently() {
    let env = TestEnv::new();
    let photos_dir = env.copy_fixtures_images_to("photos");
    let proj = env
        .services
        .project_service
        .create_project("Rescan", ProjectMode::WithoutTemplate)
        .unwrap();

    let first = env
        .services
        .photo_service
        .scan_directory(&proj.id, &photos_dir)
        .unwrap();
    assert_eq!(first.assets.len(), 9);

    std::fs::remove_file(photos_dir.join("joao_silva.jpg")).unwrap();
    std::fs::write(photos_dir.join("nova_foto.png"), MINIMAL_PNG_2X2).unwrap();

    let second = env
        .services
        .photo_service
        .scan_directory(&proj.id, &photos_dir)
        .unwrap();
    assert_eq!(second.assets.len(), 9);
    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.assets.len(), 9);
    assert!(snap.assets.iter().any(|a| a.file_name == "nova_foto.png"));
    assert!(!snap.assets.iter().any(|a| a.file_name == "joao_silva.jpg"));
}

// ============================================================================
// F-14: Bounded On-Demand Thumbnails (5 tests)
// ============================================================================

#[test]
fn t1_f14_01_decode_jpeg_png_tiff_to_96x96_rgba() {
    let env = TestEnv::new();
    for (id, rel) in [
        ("a-jpg", "images/joao_silva.jpg"),
        ("a-png", "images/carlos_eduardo.png"),
        ("a-tif", "images/aguia_dourada.tiff"),
    ] {
        let state = request_thumbnail_blocking(&env.services, id, &fixture_path(rel)).unwrap();
        match state {
            ThumbnailState::Ready(data) => {
                assert!(data.width > 0 && data.width <= 96);
                assert!(data.height > 0 && data.height <= 96);
                assert_eq!(data.rgba8.len(), (data.width * data.height * 4) as usize);
            }
            other => panic!("expected Ready thumbnail for {rel}, got {other:?}"),
        }
    }
}

#[test]
fn t1_f14_02_cache_hit_on_repeated_request() {
    let env = TestEnv::new();
    let path = fixture_path("images/joao_silva.jpg");
    let s1 = request_thumbnail_blocking(&env.services, "hit-1", &path).unwrap();
    let s2 = request_thumbnail_blocking(&env.services, "hit-1", &path).unwrap();

    match (s1, s2) {
        (ThumbnailState::Ready(d1), ThumbnailState::Ready(d2)) => {
            assert!(Arc::ptr_eq(&d1, &d2));
            assert_eq!(env.services.photo_service.thumbnail_cache_len().unwrap(), 1);
        }
        other => panic!("expected Ready hit pair, got {other:?}"),
    }
}

#[test]
fn t1_f14_03_lru_eviction_never_exceeds_192_entries() {
    let env = TestEnv::new();
    let dir = env.path().join("synth_250");
    let paths = generate_synthetic_photo_dir(&dir, 250);
    for (idx, p) in paths.iter().enumerate() {
        let state = request_thumbnail_blocking(&env.services, &format!("id-{idx}"), p).unwrap();
        assert!(matches!(state, ThumbnailState::Ready(_)));
        assert!(env.services.photo_service.thumbnail_cache_len().unwrap() <= 192);
    }
    let len = env.services.photo_service.thumbnail_cache_len().unwrap();
    assert_eq!(len, 192);
}

#[test]
fn t1_f14_04_clear_cache_drops_entries_cleanly() {
    let env = TestEnv::new();
    let dir = env.path().join("synth_10");
    let paths = generate_synthetic_photo_dir(&dir, 10);
    for (idx, p) in paths.iter().enumerate() {
        let state = request_thumbnail_blocking(&env.services, &format!("clr-{idx}"), p).unwrap();
        assert!(matches!(state, ThumbnailState::Ready(_)));
    }
    assert!(env.services.photo_service.thumbnail_cache_len().unwrap() > 0);
    env.services.photo_service.clear_thumbnail_cache().unwrap();
    assert_eq!(env.services.photo_service.thumbnail_cache_len().unwrap(), 0);
}

#[test]
fn t1_f14_05_non_square_image_scales_cleanly_within_bounds() {
    let env = TestEnv::new();
    let wide = env.path().join("wide.png");
    let tall = env.path().join("tall.png");
    generate_non_square_png(&wide, 320, 160);
    generate_non_square_png(&tall, 120, 400);
    let wide_before = snapshot_bytes(&wide);
    let tall_before = snapshot_bytes(&tall);

    for (id, p) in [("wide", &wide), ("tall", &tall)] {
        match request_thumbnail_blocking(&env.services, id, p).unwrap() {
            ThumbnailState::Ready(data) => {
                assert!(data.width > 0 && data.width <= 96);
                assert!(data.height > 0 && data.height <= 96);
                assert_eq!(data.rgba8.len(), (data.width * data.height * 4) as usize);
            }
            other => panic!("expected Ready non-square thumbnail, got {other:?}"),
        }
    }
    assert_file_unchanged(&wide, &wide_before);
    assert_file_unchanged(&tall, &tall_before);
}

// ============================================================================
// F-15: Method A: Auto Photo Matching (5 tests)
// ============================================================================

#[test]
fn t1_f15_01_exact_normalized_diacritic_case_whitespace_match() {
    let env = TestEnv::new();
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Auto Exato", &["  marcela   gonçalves  "]);
    env.services
        .photo_service
        .scan_directory(&pid, &fixture_path("images"))
        .unwrap();
    let diag = env.services.photo_service.run_auto_match(&pid).unwrap();
    assert_eq!(diag.matched_count, 1);

    let snap = env.services.project_service.open_project(&pid).unwrap();
    let val = snap.value_for(&recs[0].id, &v_foto.id).unwrap();
    assert_eq!(val.match_status, MatchStatus::AutoExact);
    assert_eq!(val.text_value, "  marcela   gonçalves  ");
    let asset = snap.asset_by_id(val.asset_id.as_deref().unwrap()).unwrap();
    assert_eq!(asset.file_name, "MARCELA  GONÇALVES.jpg");
}

#[test]
fn t1_f15_02_strip_typed_extension_in_lookup_value() {
    let env = TestEnv::new();
    let (pid, _vn, v_foto, recs) =
        setup_photo_project(&env, "Extensão Digitada", &["carlos_eduardo.png", "maria_conceicao.PNG"]);
    env.services
        .photo_service
        .scan_directory(&pid, &fixture_path("images"))
        .unwrap();
    env.services.photo_service.run_auto_match(&pid).unwrap();

    let snap = env.services.project_service.open_project(&pid).unwrap();
    let v0 = snap.value_for(&recs[0].id, &v_foto.id).unwrap();
    assert_eq!(v0.match_status, MatchStatus::AutoExact);
    assert_eq!(v0.text_value, "carlos_eduardo.png");
    assert_eq!(
        snap.asset_by_id(v0.asset_id.as_deref().unwrap()).unwrap().file_name,
        "carlos_eduardo.png"
    );

    let v1 = snap.value_for(&recs[1].id, &v_foto.id).unwrap();
    assert_eq!(v1.match_status, MatchStatus::AutoExact);
    assert_eq!(v1.text_value, "maria_conceicao.PNG");
    assert_eq!(
        snap.asset_by_id(v1.asset_id.as_deref().unwrap()).unwrap().file_name,
        "maria_conceicao.jpeg"
    );
}

#[test]
fn t1_f15_03_fuzzy_suggestion_via_jaro_winkler() {
    let env = TestEnv::new();
    let photos_dir = env.path().join("fuzzy_photos");
    std::fs::create_dir_all(&photos_dir).unwrap();
    std::fs::write(
        photos_dir.join("Roberto Henrrique Martins.jpg"),
        MINIMAL_JPEG_2X2,
    )
    .unwrap();

    let (pid, _vn, v_foto, recs) =
        setup_photo_project(&env, "Fuzzy", &["Roberto Henrique Martins"]);
    env.services
        .photo_service
        .scan_directory(&pid, &photos_dir)
        .unwrap();
    let diag = env.services.photo_service.run_auto_match(&pid).unwrap();
    assert_eq!(diag.suggested_count, 1);

    let snap = env.services.project_service.open_project(&pid).unwrap();
    let val = snap.value_for(&recs[0].id, &v_foto.id).unwrap();
    assert_eq!(val.match_status, MatchStatus::AutoSuggested);
    let cand = snap
        .match_candidates
        .iter()
        .find(|c| c.record_id == recs[0].id && c.variable_id == v_foto.id)
        .expect("fuzzy candidate recorded");
    assert!(cand.score >= 0.86);
    assert_eq!(cand.reason, REASON_FUZZY_STEM);
}

#[test]
fn t1_f15_04_accept_suggestion_promotes_to_linked() {
    let env = TestEnv::new();
    let photos_dir = env.path().join("fuzzy_accept");
    std::fs::create_dir_all(&photos_dir).unwrap();
    std::fs::write(
        photos_dir.join("Roberto Henrrique Martins.jpg"),
        MINIMAL_JPEG_2X2,
    )
    .unwrap();

    let (pid, _vn, v_foto, recs) =
        setup_photo_project(&env, "Aceitar Sugestão", &["Roberto Henrique Martins"]);
    env.services
        .photo_service
        .scan_directory(&pid, &photos_dir)
        .unwrap();
    env.services.photo_service.run_auto_match(&pid).unwrap();

    let snap_before = env.services.project_service.open_project(&pid).unwrap();
    let cand = snap_before
        .match_candidates
        .iter()
        .find(|c| c.record_id == recs[0].id && c.variable_id == v_foto.id)
        .unwrap()
        .clone();

    env.services
        .photo_service
        .accept_match_candidate(&pid, &recs[0].id, &v_foto.id, &cand.asset_id)
        .unwrap();

    let snap_after = env.services.project_service.open_project(&pid).unwrap();
    let val = snap_after.value_for(&recs[0].id, &v_foto.id).unwrap();
    assert_eq!(val.asset_id.as_deref(), Some(cand.asset_id.as_str()));
    assert!(val.match_status.is_confirmed_link());
}

#[test]
fn t1_f15_05_same_stem_multi_extension_triggers_ambiguous() {
    let env = TestEnv::new();
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Empate", &["Ana Silva"]);
    env.services
        .photo_service
        .scan_directory(&pid, &fixture_path("images"))
        .unwrap();
    let diag = env.services.photo_service.run_auto_match(&pid).unwrap();
    assert_eq!(diag.ambiguous_count, 1);

    let snap = env.services.project_service.open_project(&pid).unwrap();
    let val = snap.value_for(&recs[0].id, &v_foto.id).unwrap();
    assert_eq!(val.match_status, MatchStatus::Ambiguous);
    assert!(val.asset_id.is_none());

    let cands: Vec<&MatchCandidate> = snap
        .match_candidates
        .iter()
        .filter(|c| c.record_id == recs[0].id && c.variable_id == v_foto.id)
        .collect();
    assert_eq!(cands.len(), 2);
    assert!(cands.iter().all(|c| c.reason == REASON_AMBIGUOUS_TIE));
}

// ============================================================================
// F-16: Method B: Manual Individual Link (5 tests)
// ============================================================================

#[test]
fn t1_f16_01_manual_link_assigns_asset_and_sets_manual_status() {
    let env = TestEnv::new();
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Manual Link", &["Sem Correspondencia"]);
    env.services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    let snap = env.services.project_service.open_project(&pid).unwrap();
    let val = snap.value_for(&recs[0].id, &v_foto.id).unwrap();
    assert_eq!(val.match_status, MatchStatus::Manual);
    assert!(val.asset_id.is_some());
}

#[test]
fn t1_f16_02_manual_link_overrides_unmatched_or_suggested() {
    let env = TestEnv::new();
    let photos_dir = env.path().join("manual_override");
    std::fs::create_dir_all(&photos_dir).unwrap();
    std::fs::write(
        photos_dir.join("Roberto Henrrique Martins.jpg"),
        MINIMAL_JPEG_2X2,
    )
    .unwrap();
    std::fs::write(photos_dir.join("escolha_certa.png"), MINIMAL_PNG_2X2).unwrap();

    let (pid, _vn, v_foto, recs) =
        setup_photo_project(&env, "Manual Override", &["Roberto Henrique Martins"]);
    env.services
        .photo_service
        .scan_directory(&pid, &photos_dir)
        .unwrap();
    env.services.photo_service.run_auto_match(&pid).unwrap();

    env.services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &photos_dir.join("escolha_certa.png"))
        .unwrap();

    let snap = env.services.project_service.open_project(&pid).unwrap();
    let val = snap.value_for(&recs[0].id, &v_foto.id).unwrap();
    assert_eq!(val.match_status, MatchStatus::Manual);
    let asset = snap.asset_by_id(val.asset_id.as_deref().unwrap()).unwrap();
    assert_eq!(asset.file_name, "escolha_certa.png");
    assert!(
        snap.match_candidates
            .iter()
            .filter(|c| c.record_id == recs[0].id && c.variable_id == v_foto.id)
            .count()
            == 0
    );
}

#[test]
fn t1_f16_03_manual_link_resolves_ambiguous_tie() {
    let env = TestEnv::new();
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Resolver Empate", &["Ana Silva"]);
    env.services
        .photo_service
        .scan_directory(&pid, &fixture_path("images"))
        .unwrap();
    env.services.photo_service.run_auto_match(&pid).unwrap();

    env.services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &fixture_path("images/Ana Silva.png"))
        .unwrap();

    let snap = env.services.project_service.open_project(&pid).unwrap();
    let val = snap.value_for(&recs[0].id, &v_foto.id).unwrap();
    assert_eq!(val.match_status, MatchStatus::Manual);
    let asset = snap.asset_by_id(val.asset_id.as_deref().unwrap()).unwrap();
    assert_eq!(asset.file_name, "Ana Silva.png");
}

#[test]
fn t1_f16_04_manual_link_outside_scanned_root_registers_asset() {
    let env = TestEnv::new();
    let photos_dir = env.copy_fixtures_images_to("photos");
    let extra_path = env.path().join("extra").join("externa.png");
    std::fs::create_dir_all(extra_path.parent().unwrap()).unwrap();
    std::fs::write(&extra_path, MINIMAL_PNG_2X2).unwrap();

    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Foto Externa", &["Alguem"]);
    env.services
        .photo_service
        .scan_directory(&pid, &photos_dir)
        .unwrap();
    env.services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &extra_path)
        .unwrap();

    let snap = env.services.project_service.open_project(&pid).unwrap();
    let val = snap.value_for(&recs[0].id, &v_foto.id).unwrap();
    assert_eq!(val.match_status, MatchStatus::Manual);
    let asset = snap.asset_by_id(val.asset_id.as_deref().unwrap()).unwrap();
    assert_eq!(asset.file_name, "externa.png");
}

#[test]
fn t1_f16_05_unlink_manual_photo_restores_unmatched() {
    let env = TestEnv::new();
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Desvincular", &["Joao"]);
    env.services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();
    env.services
        .photo_service
        .unlink_photo(&pid, &recs[0].id, &v_foto.id)
        .unwrap();

    let snap = env.services.project_service.open_project(&pid).unwrap();
    let val = snap.value_for(&recs[0].id, &v_foto.id).unwrap();
    assert!(val.asset_id.is_none());
    assert_eq!(val.match_status, MatchStatus::Unmatched);
}

// ============================================================================
// F-17: Method C: Batch Unmatched Link (5 tests)
// ============================================================================

#[test]
fn t1_f17_01_query_unmatched_photos_and_unmatched_records() {
    let env = TestEnv::new();
    let photos_dir = env.path().join("batch_7");
    generate_synthetic_photo_dir(&photos_dir, 7);

    let (pid, _vn, v_foto, _recs) = setup_photo_project(
        &env,
        "Pool Lote",
        &["Pessoa_0001", "Pessoa_0002", "Pessoa_0003", "SemFoto_A", "SemFoto_B"],
    );
    env.services
        .photo_service
        .scan_directory(&pid, &photos_dir)
        .unwrap();
    env.services.photo_service.run_auto_match(&pid).unwrap();

    let pool = env
        .services
        .photo_service
        .get_unmatched_pool(&pid, &v_foto.id)
        .unwrap();
    assert_eq!(pool.records.len(), 2);
    assert_eq!(pool.assets.len(), 4);
}

#[test]
fn t1_f17_02_batch_pair_1_to_1_multiple_items() {
    let env = TestEnv::new();
    let photos_dir = env.path().join("batch_pair");
    generate_synthetic_photo_dir(&photos_dir, 7);

    let (pid, _vn, v_foto, _recs) = setup_photo_project(
        &env,
        "Parear Lote",
        &["Pessoa_0001", "Pessoa_0002", "Pessoa_0003", "SemFoto_A", "SemFoto_B"],
    );
    env.services
        .photo_service
        .scan_directory(&pid, &photos_dir)
        .unwrap();
    env.services.photo_service.run_auto_match(&pid).unwrap();

    let pool = env
        .services
        .photo_service
        .get_unmatched_pool(&pid, &v_foto.id)
        .unwrap();
    let pairs = vec![
        (pool.records[0].id.clone(), v_foto.id.clone(), pool.assets[0].id.clone()),
        (pool.records[1].id.clone(), v_foto.id.clone(), pool.assets[1].id.clone()),
    ];
    let res = env
        .services
        .photo_service
        .batch_link(&pid, &pairs, false)
        .unwrap();
    assert_eq!(res.linked_count, 2);
    assert!(res.duplicate_asset_warnings.is_empty());

    let after = env
        .services
        .photo_service
        .get_unmatched_pool(&pid, &v_foto.id)
        .unwrap();
    assert!(after.records.is_empty());
    assert_eq!(after.assets.len(), 2);
}

#[test]
fn t1_f17_03_batch_sequential_zip_pairing() {
    let env = TestEnv::new();
    let photos_dir = env.path().join("batch_zip");
    generate_synthetic_photo_dir(&photos_dir, 3);

    let (pid, _vn, v_foto, recs) =
        setup_photo_project(&env, "Zip Sequencial", &["Alpha", "Beta", "Gamma"]);
    env.services
        .photo_service
        .scan_directory(&pid, &photos_dir)
        .unwrap();

    let pool = env
        .services
        .photo_service
        .get_unmatched_pool(&pid, &v_foto.id)
        .unwrap();
    assert_eq!(pool.records.len(), 3);
    assert_eq!(pool.assets.len(), 3);

    let pairs: Vec<(String, String, String)> = recs
        .iter()
        .zip(pool.assets.iter())
        .map(|(r, a)| (r.id.clone(), v_foto.id.clone(), a.id.clone()))
        .collect();
    let res = env
        .services
        .photo_service
        .batch_link(&pid, &pairs, false)
        .unwrap();
    assert_eq!(res.linked_count, 3);

    let snap = env.services.project_service.open_project(&pid).unwrap();
    for (r, a) in recs.iter().zip(pool.assets.iter()) {
        let val = snap.value_for(&r.id, &v_foto.id).unwrap();
        assert_eq!(val.asset_id.as_deref(), Some(a.id.as_str()));
        assert_eq!(val.match_status, MatchStatus::Manual);
    }
}

#[test]
fn t1_f17_04_batch_assign_warns_if_asset_already_used() {
    let env = TestEnv::new();
    let photos_dir = env.path().join("batch_dup");
    generate_synthetic_photo_dir(&photos_dir, 2);

    let (pid, _vn, v_foto, recs) =
        setup_photo_project(&env, "Duplicar Foto", &["Pessoa_0001", "SemFoto"]);
    env.services
        .photo_service
        .scan_directory(&pid, &photos_dir)
        .unwrap();
    env.services.photo_service.run_auto_match(&pid).unwrap();

    let snap = env.services.project_service.open_project(&pid).unwrap();
    let shared_asset_id = snap
        .value_for(&recs[0].id, &v_foto.id)
        .unwrap()
        .asset_id
        .clone()
        .unwrap();

    let res = env
        .services
        .photo_service
        .batch_link(
            &pid,
            &[(recs[1].id.clone(), v_foto.id.clone(), shared_asset_id.clone())],
            false,
        )
        .unwrap();
    assert_eq!(res.linked_count, 1);
    let warn = res
        .duplicate_asset_warnings
        .iter()
        .find(|w| w.asset_id == shared_asset_id)
        .expect("duplicate asset warning surfaced");
    assert!(warn.record_ids.contains(&recs[0].id));
    assert!(warn.record_ids.contains(&recs[1].id));
}

#[test]
fn t1_f17_05_batch_pairing_persists_across_reopen() {
    let env = TestEnv::new();
    let photos_dir = env.path().join("batch_reopen");
    generate_synthetic_photo_dir(&photos_dir, 4);

    let (pid, _vn, v_foto, recs) =
        setup_photo_project(&env, "Batch Reopen", &["X1", "X2", "X3"]);
    env.services
        .photo_service
        .scan_directory(&pid, &photos_dir)
        .unwrap();
    let pool = env
        .services
        .photo_service
        .get_unmatched_pool(&pid, &v_foto.id)
        .unwrap();
    let pairs = vec![
        (recs[0].id.clone(), v_foto.id.clone(), pool.assets[0].id.clone()),
        (recs[1].id.clone(), v_foto.id.clone(), pool.assets[1].id.clone()),
    ];
    env.services
        .photo_service
        .batch_link(&pid, &pairs, false)
        .unwrap();

    let reopened = env.reopen();
    let snap = reopened.project_service.open_project(&pid).unwrap();
    assert_eq!(
        snap.value_for(&recs[0].id, &v_foto.id).unwrap().asset_id.as_deref(),
        Some(pool.assets[0].id.as_str())
    );
    assert_eq!(
        snap.value_for(&recs[1].id, &v_foto.id).unwrap().match_status,
        MatchStatus::Manual
    );
    let pool_after = reopened
        .photo_service
        .get_unmatched_pool(&pid, &v_foto.id)
        .unwrap();
    assert_eq!(pool_after.records.len(), 1);
    assert_eq!(pool_after.assets.len(), 2);
}

// ============================================================================
// F-18: Photo Diagnostics (5 tests)
// ============================================================================

#[test]
fn t1_f18_01_detects_records_missing_photo() {
    let env = TestEnv::new();
    let photos_dir = env.path().join("diag_missing");
    generate_synthetic_photo_dir(&photos_dir, 3);

    let (pid, _vn, _vf, recs) = setup_photo_project(
        &env,
        "Diag Sem Foto",
        &["Pessoa_0001", "Pessoa_0002", "Pessoa_0003", "SemFoto_1", "SemFoto_2"],
    );
    env.services
        .photo_service
        .scan_directory(&pid, &photos_dir)
        .unwrap();
    env.services.photo_service.run_auto_match(&pid).unwrap();

    let diag = env
        .services
        .photo_service
        .compute_diagnostics(&pid, false)
        .unwrap();
    assert_eq!(diag.records_missing_photo.len(), 2);
    assert!(diag.records_missing_photo.contains(&recs[3].id));
    assert!(diag.records_missing_photo.contains(&recs[4].id));
}

#[test]
fn t1_f18_02_detects_unmatched_orphan_photos() {
    let env = TestEnv::new();
    let photos_dir = env.path().join("diag_orphan");
    generate_synthetic_photo_dir(&photos_dir, 5);

    let (pid, _vn, _vf, _recs) = setup_photo_project(
        &env,
        "Diag Órfãs",
        &["Pessoa_0001", "Pessoa_0002", "Pessoa_0003"],
    );
    env.services
        .photo_service
        .scan_directory(&pid, &photos_dir)
        .unwrap();
    env.services.photo_service.run_auto_match(&pid).unwrap();

    let diag = env
        .services
        .photo_service
        .compute_diagnostics(&pid, false)
        .unwrap();
    assert_eq!(diag.unmatched_asset_ids.len(), 2);
}

#[test]
fn t1_f18_03_detects_duplicate_photo_assignments() {
    let env = TestEnv::new();
    let img = fixture_path("images/joao_silva.jpg");
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Diag Dup", &["R1", "R2"]);
    env.services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &img)
        .unwrap();
    env.services
        .photo_service
        .link_manual(&pid, &recs[1].id, &v_foto.id, &img)
        .unwrap();

    let diag = env
        .services
        .photo_service
        .compute_diagnostics(&pid, false)
        .unwrap();
    assert_eq!(diag.duplicate_asset_assignments.len(), 1);
    let dup = &diag.duplicate_asset_assignments[0];
    assert!(dup.record_ids.contains(&recs[0].id));
    assert!(dup.record_ids.contains(&recs[1].id));
}

#[test]
fn t1_f18_04_verifies_physical_disk_existence_refresh() {
    let env = TestEnv::new();
    let photos_dir = env.copy_fixtures_images_to("diag_disk");
    let target = photos_dir.join("joao_silva.jpg");
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Diag Disco", &["Joao"]);
    env.services
        .photo_service
        .scan_directory(&pid, &photos_dir)
        .unwrap();
    env.services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &target)
        .unwrap();

    let snap_before = env.services.project_service.open_project(&pid).unwrap();
    let asset_id = snap_before
        .value_for(&recs[0].id, &v_foto.id)
        .unwrap()
        .asset_id
        .clone()
        .unwrap();

    std::fs::remove_file(&target).unwrap();
    let diag = env
        .services
        .photo_service
        .compute_diagnostics(&pid, true)
        .unwrap();
    assert_eq!(diag.missing_on_disk_asset_ids, vec![asset_id.clone()]);

    let snap_after = env.services.project_service.open_project(&pid).unwrap();
    assert!(!snap_after.asset_by_id(&asset_id).unwrap().exists_on_disk);
}

#[test]
fn t1_f18_05_restored_file_clears_missing_on_disk_flag() {
    let env = TestEnv::new();
    let photos_dir = env.copy_fixtures_images_to("diag_restore");
    let target = photos_dir.join("joao_silva.jpg");
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Diag Restore", &["Joao"]);
    env.services
        .photo_service
        .scan_directory(&pid, &photos_dir)
        .unwrap();
    env.services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &target)
        .unwrap();

    std::fs::remove_file(&target).unwrap();
    let diag1 = env
        .services
        .photo_service
        .compute_diagnostics(&pid, true)
        .unwrap();
    assert_eq!(diag1.missing_on_disk_asset_ids.len(), 1);
    let asset_id = diag1.missing_on_disk_asset_ids[0].clone();

    std::fs::write(&target, MINIMAL_JPEG_2X2).unwrap();
    let diag2 = env
        .services
        .photo_service
        .compute_diagnostics(&pid, true)
        .unwrap();
    assert!(diag2.missing_on_disk_asset_ids.is_empty());
    let snap = env.services.project_service.open_project(&pid).unwrap();
    assert!(snap.asset_by_id(&asset_id).unwrap().exists_on_disk);
}

// ============================================================================
// F-19: Modo XML com Modelo (5 tests)
// ============================================================================

#[test]
fn t1_f19_01_inspect_standard_illustrator_template() {
    let env = TestEnv::new();

    let mixed = env
        .services
        .export_service
        .inspect_xml_template(&read_fixture_string("template_mixed.xml"))
        .unwrap();
    assert_eq!(mixed.var_set_name.as_deref(), Some("ModeloMisto"));
    assert_eq!(mixed.sample_datasets_count, 1);
    let mixed_names: Vec<&str> = mixed.variables.iter().map(|v| v.var_name.as_str()).collect();
    assert_eq!(mixed_names, vec!["Nome", "Funcao", "Matricula", "Foto"]);
    assert_eq!(mixed.variables[3].trait_kind, TemplateVariableTrait::Fileref);

    let text = env
        .services
        .export_service
        .inspect_xml_template(&read_fixture_string("template_text.xml"))
        .unwrap();
    assert_eq!(text.var_set_name.as_deref(), Some("ModeloTexto"));
    assert_eq!(text.sample_datasets_count, 0);
    assert_eq!(text.variables.len(), 3);

    let img = env
        .services
        .export_service
        .inspect_xml_template(&read_fixture_string("template_image.xml"))
        .unwrap();
    assert_eq!(img.var_set_name.as_deref(), Some("ModeloImagem"));
    assert_eq!(img.variables.len(), 1);
    assert_eq!(img.variables[0].var_name, "Foto");
    assert_eq!(img.variables[0].trait_kind, TemplateVariableTrait::Fileref);
}

#[test]
fn t1_f19_02_preserve_doctype_6_entities_and_headers_verbatim() {
    let env = TestEnv::new();
    let tpl_raw = read_fixture_string("template_mixed.xml");
    let proj = env
        .services
        .project_service
        .create_project("Preservar Cabeçalho", ProjectMode::WithTemplate)
        .unwrap();
    env.services
        .export_service
        .bind_xml_template(&proj.id, Some(&fixture_path("template_mixed.xml")), &tpl_raw)
        .unwrap();

    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let v_func = env
        .services
        .project_service
        .create_variable(&proj.id, "Funcao", Some("Funcao"), VariableKind::Text, false)
        .unwrap();
    let v_mat = env
        .services
        .project_service
        .create_variable(&proj.id, "Matricula", Some("Matricula"), VariableKind::Text, false)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&proj.id, "Foto", Some("Foto"), VariableKind::LinkedFile, false)
        .unwrap();

    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[
            ("01 - Ana".into(), vec![(&v_nome, "Ana"), (&v_func, "Dev"), (&v_mat, "001")]),
            ("02 - Bruno".into(), vec![(&v_nome, "Bruno"), (&v_func, "QA"), (&v_mat, "002")]),
        ],
    );
    env.services
        .photo_service
        .link_manual(&proj.id, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();
    env.services
        .photo_service
        .link_manual(&proj.id, &recs[1].id, &v_foto.id, &fixture_path("images/carlos_eduardo.png"))
        .unwrap();

    let generated = env
        .services
        .export_service
        .generate_xml_preview(&proj.id)
        .unwrap();
    assert_illustrator_xml_valid(&generated, 2);

    let tpl_start = tpl_raw.find("<v:sampleDataSets").unwrap();
    let gen_start = generated.find("<v:sampleDataSets").unwrap();
    assert_eq!(&generated[..gen_start], &tpl_raw[..tpl_start]);

    let tpl_end = tpl_raw.find("</v:sampleDataSets>").unwrap();
    let gen_end = generated.find("</v:sampleDataSets>").unwrap();
    assert_eq!(&generated[gen_end..], &tpl_raw[tpl_end..]);
}

#[test]
fn t1_f19_03_preserve_c2pa_metadata_and_double_spaces() {
    let env = TestEnv::new();
    let tpl_raw = read_fixture_string("segunda_leva_variant.xml");
    let proj = env
        .services
        .project_service
        .create_project("C2PA Preservar", ProjectMode::WithTemplate)
        .unwrap();
    let parsed = env
        .services
        .export_service
        .bind_xml_template(
            &proj.id,
            Some(&fixture_path("segunda_leva_variant.xml")),
            &tpl_raw,
        )
        .unwrap();
    assert!(parsed.has_c2pa_metadata);

    let v_nome = env
        .services
        .project_service
        .create_variable(
            &proj.id,
            "NomeFuncionario",
            Some("NomeFuncionario"),
            VariableKind::Text,
            true,
        )
        .unwrap();
    let v_func = env
        .services
        .project_service
        .create_variable(
            &proj.id,
            "FuncaoFuncionario",
            Some("FuncaoFuncionario"),
            VariableKind::Text,
            false,
        )
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(
            &proj.id,
            "FotoFuncionario",
            Some("FotoFuncionario"),
            VariableKind::LinkedFile,
            false,
        )
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01".into(), vec![(&v_nome, "Beatriz"), (&v_func, "Cozinheira")])],
    );
    env.services
        .photo_service
        .link_manual(
            &proj.id,
            &recs[0].id,
            &v_foto.id,
            &fixture_path("images/maria_conceicao.jpeg"),
        )
        .unwrap();

    let xml = env.services.export_service.generate_xml_preview(&proj.id).unwrap();
    assert!(xml.contains("<c2pa:manifest>AAAWgmp1bWJHZW5lcmF0ZWRTeW50aGV0aWNDMlBBTWFuaWZlc3REYXRh</c2pa:manifest>"));
    assert!(xml.contains("<variableSets  xmlns=\"&ns_vars;\">"));
    assert!(xml.contains("<variableSet  locked=\"none\" varSetName=\"CrachasCorporativos\">"));
    assert_illustrator_xml_valid(&xml, 1);
}

#[test]
fn t1_f19_04_handle_legacy_trait_textcontent_on_image_variable() {
    let env = TestEnv::new();
    let tpl_raw = read_fixture_string("bombeiro_reference.xml");
    let parsed = env
        .services
        .export_service
        .inspect_xml_template(&tpl_raw)
        .unwrap();
    assert_eq!(parsed.var_set_name.as_deref(), Some("binding1"));
    assert_eq!(parsed.variables[2].var_name, "foto");
    assert_eq!(parsed.variables[2].trait_kind, TemplateVariableTrait::TextContent);
    assert_eq!(parsed.variables[2].sample_uses_p_wrapper, Some(false));

    let proj = env
        .services
        .project_service
        .create_project("Bombeiro Legado", ProjectMode::WithTemplate)
        .unwrap();
    env.services
        .export_service
        .bind_xml_template(&proj.id, Some(&fixture_path("bombeiro_reference.xml")), &tpl_raw)
        .unwrap();

    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "nome", Some("nome"), VariableKind::Text, true)
        .unwrap();
    let v_func = env
        .services
        .project_service
        .create_variable(&proj.id, "funcao", Some("funcao"), VariableKind::Text, false)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&proj.id, "foto", Some("foto"), VariableKind::LinkedFile, false)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[("Lucas".into(), vec![(&v_nome, "Lucas"), (&v_func, "")])],
    );
    env.services
        .photo_service
        .link_manual(&proj.id, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    let xml = env.services.export_service.generate_xml_preview(&proj.id).unwrap();
    assert_illustrator_xml_matches_columns(&xml, 1, &["nome", "funcao"], &["foto"]);
}

#[test]
fn t1_f19_05_preserve_out_of_scope_visibility_graphdata_variables() {
    let env = TestEnv::new();
    let tpl_raw = read_fixture_string("template_visibility_graph.xml");
    let proj = env
        .services
        .project_service
        .create_project("Extras", ProjectMode::WithTemplate)
        .unwrap();
    let parsed = env
        .services
        .export_service
        .bind_xml_template(
            &proj.id,
            Some(&fixture_path("template_visibility_graph.xml")),
            &tpl_raw,
        )
        .unwrap();
    assert!(parsed
        .variables
        .iter()
        .any(|v| v.var_name == "SeloVisivel" && v.trait_kind == TemplateVariableTrait::Visibility));
    assert!(parsed
        .variables
        .iter()
        .any(|v| v.var_name == "GraficoDesempenho" && v.trait_kind == TemplateVariableTrait::GraphData));

    let v_tit = env
        .services
        .project_service
        .create_variable(&proj.id, "Titulo", Some("Titulo"), VariableKind::Text, true)
        .unwrap();
    let v_img = env
        .services
        .project_service
        .create_variable(&proj.id, "Imagem", Some("Imagem"), VariableKind::LinkedFile, false)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[("Relatório".into(), vec![(&v_tit, "Relatório Abril")])],
    );
    env.services
        .photo_service
        .link_manual(&proj.id, &recs[0].id, &v_img.id, &fixture_path("images/carlos_eduardo.png"))
        .unwrap();

    let xml = env.services.export_service.generate_xml_preview(&proj.id).unwrap();
    assert!(xml.contains("trait=\"visibility\" varName=\"SeloVisivel\""));
    assert!(xml.contains("trait=\"graphdata\" varName=\"GraficoDesempenho\""));
    assert_illustrator_xml_valid(&xml, 1);
}

// ============================================================================
// F-20: Modo Dados sem Modelo (5 tests)
// ============================================================================

#[test]
fn t1_f20_01_edit_and_persist_without_xml_template() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Sem Modelo", ProjectMode::WithoutTemplate)
        .unwrap();
    let v1 = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let _v2 = env
        .services
        .project_service
        .create_variable(&proj.id, "Foto", Some("Foto"), VariableKind::LinkedFile, false)
        .unwrap();

    for i in 0..5 {
        let r = env
            .services
            .data_service
            .add_record(&proj.id, Some(&format!("Reg_{i}")))
            .unwrap();
        env.services
            .data_service
            .update_cell(&proj.id, &r.id, &v1.id, &format!("Pessoa {i}"))
            .unwrap();
    }

    let reopened = env.reopen();
    let snap = reopened.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.project.mode, ProjectMode::WithoutTemplate);
    assert!(snap.project.xml_template_raw.is_none());
    assert_eq!(snap.records.len(), 5);
}

#[test]
fn t1_f20_02_export_standard_illustrator_envelope_directly() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Envelope Direto", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&proj.id, "Foto", Some("Foto"), VariableKind::LinkedFile, true)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01".into(), vec![(&v_nome, "João Silva")])],
    );
    env.services
        .photo_service
        .link_manual(&proj.id, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    let preview = env.services.export_service.generate_xml_preview(&proj.id).unwrap();
    assert_illustrator_xml_valid(&preview, 1);

    let out_dir = env.path().join("export_direct");
    env.services
        .export_service
        .export_package(&proj.id, &out_dir, false)
        .unwrap();
    let bytes = snapshot_bytes(&out_dir.join("dados.xml"));
    assert_utf8_no_bom_crlf(&bytes);
    assert_illustrator_xml_valid(&String::from_utf8(bytes).unwrap(), 1);
}

#[test]
fn t1_f20_03_late_bind_xml_template_to_existing_project() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Vínculo Tardio", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[
            ("01".into(), vec![(&v_nome, "Pessoa 1")]),
            ("02".into(), vec![(&v_nome, "Pessoa 2")]),
            ("03".into(), vec![(&v_nome, "Pessoa 3")]),
            ("04".into(), vec![(&v_nome, "Pessoa 4")]),
            ("05".into(), vec![(&v_nome, "Pessoa 5")]),
        ],
    );

    let tpl_path = fixture_path("template_mixed.xml");
    let tpl_raw = read_fixture_string("template_mixed.xml");
    let parsed = env
        .services
        .export_service
        .bind_xml_template(&proj.id, Some(&tpl_path), &tpl_raw)
        .unwrap();
    assert_eq!(parsed.var_set_name.as_deref(), Some("ModeloMisto"));

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert!(snap.project.xml_template_raw.is_some());
    assert_eq!(snap.records.len(), 5);
    assert_eq!(snap.value_for(&recs[4].id, &v_nome.id).unwrap().text_value, "Pessoa 5");
}

#[test]
fn t1_f20_04_map_project_columns_to_template_variables() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Mapeamento Template", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_a = env
        .services
        .project_service
        .create_variable(&proj.id, "Col_A", Some("Col_A"), VariableKind::Text, true)
        .unwrap();
    let v_b = env
        .services
        .project_service
        .create_variable(&proj.id, "Col_B", Some("Col_B"), VariableKind::Text, false)
        .unwrap();
    let v_c = env
        .services
        .project_service
        .create_variable(&proj.id, "Col_C", Some("Col_C"), VariableKind::Text, false)
        .unwrap();
    let v_p = env
        .services
        .project_service
        .create_variable(&proj.id, "Col_Photo", Some("Col_Photo"), VariableKind::LinkedFile, false)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01".into(), vec![(&v_a, "Ana"), (&v_b, "Dev"), (&v_c, "001")])],
    );
    env.services
        .photo_service
        .link_manual(&proj.id, &recs[0].id, &v_p.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    env.services
        .export_service
        .bind_xml_template(
            &proj.id,
            Some(&fixture_path("template_mixed.xml")),
            &read_fixture_string("template_mixed.xml"),
        )
        .unwrap();

    env.services
        .project_service
        .update_variable_options(&proj.id, &v_a.id, VariableKind::Text, true, Some("Nome".into()))
        .unwrap();
    env.services
        .project_service
        .update_variable_options(&proj.id, &v_b.id, VariableKind::Text, false, Some("Funcao".into()))
        .unwrap();
    env.services
        .project_service
        .update_variable_options(&proj.id, &v_c.id, VariableKind::Text, false, Some("Matricula".into()))
        .unwrap();
    env.services
        .project_service
        .update_variable_options(&proj.id, &v_p.id, VariableKind::LinkedFile, false, Some("Foto".into()))
        .unwrap();

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(
        snap.variable_by_id(&v_a.id).unwrap().mapped_template_var.as_deref(),
        Some("Nome")
    );
    assert_eq!(
        snap.variable_by_id(&v_p.id).unwrap().mapped_template_var.as_deref(),
        Some("Foto")
    );

    let xml = env.services.export_service.generate_xml_preview(&proj.id).unwrap();
    assert!(xml.contains("<Nome><p>Ana</p></Nome>"));
    assert!(xml.contains("<Foto>"));
    assert_illustrator_xml_valid(&xml, 1);
}

#[test]
fn t1_f20_05_rebind_different_xml_template_updates_mapping() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Rebind Template", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let v_cargo = env
        .services
        .project_service
        .create_variable(&proj.id, "Cargo", Some("Cargo"), VariableKind::Text, false)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01".into(), vec![(&v_nome, "Maria"), (&v_cargo, "Analista")])],
    );

    env.services
        .export_service
        .bind_xml_template(
            &proj.id,
            Some(&fixture_path("template_text.xml")),
            &read_fixture_string("template_text.xml"),
        )
        .unwrap();
    env.services
        .project_service
        .update_variable_options(
            &proj.id,
            &v_nome.id,
            VariableKind::Text,
            true,
            Some("NomeCompleto".into()),
        )
        .unwrap();

    let parsed2 = env
        .services
        .export_service
        .bind_xml_template(
            &proj.id,
            Some(&fixture_path("secom_reference.xml")),
            &read_fixture_string("secom_reference.xml"),
        )
        .unwrap();
    assert_eq!(parsed2.var_set_name.as_deref(), Some("CrachasCorporativos"));

    env.services
        .project_service
        .update_variable_options(
            &proj.id,
            &v_nome.id,
            VariableKind::Text,
            true,
            Some("NomeFuncionario".into()),
        )
        .unwrap();
    env.services
        .project_service
        .update_variable_options(
            &proj.id,
            &v_cargo.id,
            VariableKind::Text,
            false,
            Some("FuncaoFuncionario".into()),
        )
        .unwrap();

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.value_for(&recs[0].id, &v_nome.id).unwrap().text_value, "Maria");
    assert_eq!(
        snap.variable_by_id(&v_nome.id).unwrap().mapped_template_var.as_deref(),
        Some("NomeFuncionario")
    );

    let xml = env.services.export_service.generate_xml_preview(&proj.id).unwrap();
    assert!(xml.contains("<NomeFuncionario><p>Maria</p></NomeFuncionario>"));
    assert!(xml.contains("<FuncaoFuncionario><p>Analista</p></FuncaoFuncionario>"));
    assert_illustrator_xml_valid(&xml, 1);
}

// ============================================================================
// F-21: UTF-8 XML Emission & Path Strategies (5 tests)
// ============================================================================

#[test]
fn t1_f21_01_utf8_no_bom_and_crlf_line_endings() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("UTF8 CRLF", ProjectMode::WithTemplate)
        .unwrap();
    env.services
        .export_service
        .bind_xml_template(
            &proj.id,
            Some(&fixture_path("template_mixed.xml")),
            &read_fixture_string("template_mixed.xml"),
        )
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let _v_func = env
        .services
        .project_service
        .create_variable(&proj.id, "Funcao", Some("Funcao"), VariableKind::Text, false)
        .unwrap();
    let _v_mat = env
        .services
        .project_service
        .create_variable(&proj.id, "Matricula", Some("Matricula"), VariableKind::Text, false)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&proj.id, "Foto", Some("Foto"), VariableKind::LinkedFile, false)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01".into(), vec![(&v_nome, "José")])],
    );
    env.services
        .photo_service
        .link_manual(&proj.id, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    let out_dir = env.path().join("out_utf8");
    env.services
        .export_service
        .export_package(&proj.id, &out_dir, false)
        .unwrap();
    let bytes = snapshot_bytes(&out_dir.join("dados.xml"));
    assert_eq!(&bytes[..5], b"<?xml");
    assert_utf8_no_bom_crlf(&bytes);
}

#[test]
fn t1_f21_02_xml_reserved_char_escaping_in_text_and_attrs() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Escape XML", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let ds_raw = "A & B <1> \"X\" 'Y'";
    let text_raw = "10 < 20 & 'ok' > \"no\"";
    seed_records_and_values(
        &env.services,
        &proj.id,
        &[(ds_raw.to_string(), vec![(&v_nome, text_raw)])],
    );

    let xml = env.services.export_service.generate_xml_preview(&proj.id).unwrap();
    assert!(xml.contains("A &amp; B &lt;1&gt;"));
    assert!(xml.contains("&amp;"));
    assert!(xml.contains("&lt;"));
    assert!(xml.contains("&gt;"));
    assert!(xml.contains("&quot;"));
    assert!(xml.contains("&apos;"));
    assert_illustrator_xml_valid(&xml, 1);

    let parsed = extract_xml_datasets(&xml);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].0, ds_raw);
    assert_eq!(parsed[0].1.get("Nome").map(String::as_str), Some(text_raw));
}

#[test]
fn t1_f21_03_text_uses_p_wrapper_linked_file_uses_raw_no_p() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Wrapper P", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let v_func = env
        .services
        .project_service
        .create_variable(&proj.id, "Funcao", Some("Funcao"), VariableKind::Text, false)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&proj.id, "Foto", Some("Foto"), VariableKind::LinkedFile, true)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01".into(), vec![(&v_nome, "Ana"), (&v_func, "")])],
    );
    env.services
        .photo_service
        .link_manual(&proj.id, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    let out_dir = env.path().join("out_p");
    env.services
        .export_service
        .export_package(&proj.id, &out_dir, false)
        .unwrap();
    let xml = std::fs::read_to_string(out_dir.join("dados.xml")).unwrap();
    assert!(xml.contains("<Nome><p>Ana</p></Nome>"));
    assert!(xml.contains("<Funcao><p></p></Funcao>"));
    assert!(xml.contains("<Foto>imagens/asset_001.jpg</Foto>"));
    assert_illustrator_xml_valid(&xml, 1);
}

#[test]
fn t1_f21_04_relative_to_xml_strategy_emits_forward_slashes() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Relative Path", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&proj.id, "Foto", Some("Foto"), VariableKind::LinkedFile, true)
        .unwrap();
    let recs = seed_records_and_values(&env.services, &proj.id, &[("01".into(), vec![])]);
    env.services
        .photo_service
        .link_manual(&proj.id, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    let out_dir = env.path().join("out_rel");
    env.services
        .export_service
        .export_package(&proj.id, &out_dir, false)
        .unwrap();
    let xml = std::fs::read_to_string(out_dir.join("dados.xml")).unwrap();
    assert!(xml.contains("<Foto>imagens/asset_001.jpg</Foto>"));
    assert!(!xml.contains("imagens\\asset_001.jpg"));
}

#[test]
fn t1_f21_05_absolute_file_uri_strategy_emits_valid_file_uri() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Absolute URI", ProjectMode::WithoutTemplate)
        .unwrap();
    env.services
        .project_service
        .set_path_strategy(&proj.id, PathStrategy::AbsoluteFileUri)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&proj.id, "Foto", Some("Foto"), VariableKind::LinkedFile, true)
        .unwrap();
    let recs = seed_records_and_values(&env.services, &proj.id, &[("01".into(), vec![])]);
    env.services
        .photo_service
        .link_manual(&proj.id, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    let out_dir = env.path().join("Exports").join("Lote 01");
    env.services
        .export_service
        .export_package(&proj.id, &out_dir, false)
        .unwrap();
    let xml = std::fs::read_to_string(out_dir.join("dados.xml")).unwrap();
    let datasets = extract_xml_datasets(&xml);
    let uri = datasets[0].1.get("Foto").unwrap();
    assert!(uri.starts_with("file:///"));
    assert!(uri.contains("Lote%2001"));
    assert!(uri.ends_with("/imagens/asset_001.jpg"));
    assert!(!uri.contains('\\'));
    assert_illustrator_xml_valid(&xml, 1);
}

// ============================================================================
// F-22: Pre-Export Audit (5 tests)
// ============================================================================

#[test]
fn t1_f22_01_clean_project_passes_with_zero_blocking_errors() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Audit Limpo", ProjectMode::WithTemplate)
        .unwrap();
    env.services
        .export_service
        .bind_xml_template(
            &proj.id,
            Some(&fixture_path("template_mixed.xml")),
            &read_fixture_string("template_mixed.xml"),
        )
        .unwrap();

    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let v_func = env
        .services
        .project_service
        .create_variable(&proj.id, "Funcao", Some("Funcao"), VariableKind::Text, true)
        .unwrap();
    let v_mat = env
        .services
        .project_service
        .create_variable(&proj.id, "Matricula", Some("Matricula"), VariableKind::Text, true)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&proj.id, "Foto", Some("Foto"), VariableKind::LinkedFile, true)
        .unwrap();

    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[(
            "01 - João".into(),
            vec![(&v_nome, "João"), (&v_func, "Designer"), (&v_mat, "001")],
        )],
    );
    env.services
        .photo_service
        .link_manual(&proj.id, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    let report = env.services.validation_service.audit_project(&proj.id).unwrap();
    assert!(report.can_export);
    assert_eq!(report.blocking_count, 0);
    assert_eq!(report.warning_count, 0);
    assert!(report.issues.is_empty());
}

#[test]
fn t1_f22_02_optional_empty_cell_emits_warning_w01_not_blocking() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Audit W01", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let v_obs = env
        .services
        .project_service
        .create_variable(&proj.id, "Obs", Some("Obs"), VariableKind::Text, false)
        .unwrap();
    seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01".into(), vec![(&v_nome, "Maria"), (&v_obs, "")])],
    );

    let report = env.services.validation_service.audit_project(&proj.id).unwrap();
    assert!(report.can_export);
    assert_eq!(report.blocking_count, 0);
    assert!(report.warning_count >= 1);
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.code == ValidationCode::W01OptionalCellEmpty)
    );
}

#[test]
fn t1_f22_03_duplicate_text_values_emit_warning_w03() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Audit W03", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    seed_records_and_values(
        &env.services,
        &proj.id,
        &[
            ("01".into(), vec![(&v_nome, "Carlos Silva")]),
            ("02".into(), vec![(&v_nome, "Carlos Silva")]),
        ],
    );

    let report = env.services.validation_service.audit_project(&proj.id).unwrap();
    assert!(report.can_export);
    assert_eq!(report.blocking_count, 0);
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.code == ValidationCode::W03DuplicateKeyTextValue)
    );
}

#[test]
fn t1_f22_04_shared_photo_and_unmatched_photos_emit_w04_w05() {
    let env = TestEnv::new();
    let photos_dir = env.path().join("audit_w");
    std::fs::create_dir_all(&photos_dir).unwrap();
    std::fs::write(photos_dir.join("Roberto Henrrique Martins.jpg"), MINIMAL_JPEG_2X2).unwrap();
    std::fs::write(photos_dir.join("joao_silva.jpg"), MINIMAL_JPEG_2X2).unwrap();
    std::fs::write(photos_dir.join("sobra_orfa.png"), MINIMAL_PNG_2X2).unwrap();

    let (pid, _vn, v_foto, recs) = setup_photo_project(
        &env,
        "Audit W02 W04 W05",
        &["Roberto Henrique Martins", "R2", "R3"],
    );
    env.services
        .photo_service
        .scan_directory(&pid, &photos_dir)
        .unwrap();
    env.services.photo_service.run_auto_match(&pid).unwrap();
    let shared_img = photos_dir.join("joao_silva.jpg");
    env.services
        .photo_service
        .link_manual(&pid, &recs[1].id, &v_foto.id, &shared_img)
        .unwrap();
    env.services
        .photo_service
        .link_manual(&pid, &recs[2].id, &v_foto.id, &shared_img)
        .unwrap();

    let report = env.services.validation_service.audit_project(&pid).unwrap();
    assert!(report.can_export);
    assert_eq!(report.blocking_count, 0);
    assert!(report.issues.iter().any(|i| i.code == ValidationCode::W02SuggestedPhotoPending));
    assert!(report.issues.iter().any(|i| i.code == ValidationCode::W04SharedAssetAcrossRecords));
    assert!(report.issues.iter().any(|i| i.code == ValidationCode::W05UnmatchedScannedPhotos));

    // Subcaso W07: template_visibility_graph.xml
    let p2 = env
        .services
        .project_service
        .create_project("Audit W07", ProjectMode::WithTemplate)
        .unwrap();
    env.services
        .export_service
        .bind_xml_template(
            &p2.id,
            Some(&fixture_path("template_visibility_graph.xml")),
            &read_fixture_string("template_visibility_graph.xml"),
        )
        .unwrap();
    let v_tit = env
        .services
        .project_service
        .create_variable(&p2.id, "Titulo", Some("Titulo"), VariableKind::Text, true)
        .unwrap();
    let v_img = env
        .services
        .project_service
        .create_variable(&p2.id, "Imagem", Some("Imagem"), VariableKind::LinkedFile, false)
        .unwrap();
    let recs2 = seed_records_and_values(
        &env.services,
        &p2.id,
        &[("01".into(), vec![(&v_tit, "Capa")])],
    );
    env.services
        .photo_service
        .link_manual(&p2.id, &recs2[0].id, &v_img.id, &photos_dir.join("joao_silva.jpg"))
        .unwrap();

    let report2 = env.services.validation_service.audit_project(&p2.id).unwrap();
    assert!(
        report2
            .issues
            .iter()
            .any(|i| i.code == ValidationCode::W07IgnoredTemplateVariableType)
    );
}

#[test]
fn t1_f22_05_audit_issues_include_navigable_record_and_variable_ids() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Audit Targets", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_req = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let v_opt = env
        .services
        .project_service
        .create_variable(&proj.id, "Obs", Some("Obs"), VariableKind::Text, false)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01".into(), vec![(&v_req, ""), (&v_opt, "")])],
    );

    let report = env.services.validation_service.audit_project(&proj.id).unwrap();
    assert!(!report.can_export);
    assert_eq!(report.blocking_count, 1);

    assert!(report.issues.iter().any(|i| {
        i.code == ValidationCode::E04RequiredTextEmpty
            && i.target
                == ValidationTarget::Cell {
                    record_id: recs[0].id.clone(),
                    variable_id: v_req.id.clone(),
                }
    }));
    assert!(report.issues.iter().any(|i| {
        i.code == ValidationCode::W01OptionalCellEmpty
            && i.target
                == ValidationTarget::Cell {
                    record_id: recs[0].id.clone(),
                    variable_id: v_opt.id.clone(),
                }
    }));
}

// ============================================================================
// F-23: Atomic Portable Package Export (5 tests)
// ============================================================================

#[test]
fn t1_f23_01_exports_complete_4_part_portable_package() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Pacote 4 Partes", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&proj.id, "Foto", Some("Foto"), VariableKind::LinkedFile, false)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[
            ("01".into(), vec![(&v_nome, "João")]),
            ("02".into(), vec![(&v_nome, "Carlos")]),
            ("03".into(), vec![(&v_nome, "Sem Foto")]),
        ],
    );
    env.services
        .photo_service
        .link_manual(&proj.id, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();
    env.services
        .photo_service
        .link_manual(&proj.id, &recs[1].id, &v_foto.id, &fixture_path("images/carlos_eduardo.png"))
        .unwrap();

    let out_dir = env.path().join("pkg_4");
    let summary = env
        .services
        .export_service
        .export_package(&proj.id, &out_dir, false)
        .unwrap();
    assert_eq!(summary.exported_datasets, 3);
    assert_eq!(summary.exported_assets, 2);
    assert!(out_dir.join("dados.xml").metadata().unwrap().len() > 0);
    assert!(out_dir.join("imagens/asset_001.jpg").metadata().unwrap().len() > 0);
    assert!(out_dir.join("imagens/asset_002.png").metadata().unwrap().len() > 0);
    assert!(out_dir.join("manifest.json").metadata().unwrap().len() > 0);
    assert!(out_dir.join("relatorio.csv").metadata().unwrap().len() > 0);
}

#[test]
fn t1_f23_02_copies_only_linked_photos_with_deterministic_asset_names() {
    let env = TestEnv::new();
    let photos_dir = env.copy_fixtures_images_to("photos_9");
    let p1 = photos_dir.join("joao_silva.jpg");
    let p2 = photos_dir.join("carlos_eduardo.png");
    let b1 = snapshot_bytes(&p1);
    let b2 = snapshot_bytes(&p2);

    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Somente Vinculadas", &["A", "B"]);
    env.services
        .photo_service
        .scan_directory(&pid, &photos_dir)
        .unwrap();
    env.services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &p1)
        .unwrap();
    env.services
        .photo_service
        .link_manual(&pid, &recs[1].id, &v_foto.id, &p2)
        .unwrap();

    let out_dir = env.path().join("pkg_linked_only");
    env.services
        .export_service
        .export_package(&pid, &out_dir, false)
        .unwrap();

    let img_entries: Vec<PathBuf> = std::fs::read_dir(out_dir.join("imagens"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(img_entries.len(), 2);
    assert_eq!(snapshot_bytes(&out_dir.join("imagens/asset_001.jpg")), b1);
    assert_eq!(snapshot_bytes(&out_dir.join("imagens/asset_002.png")), b2);
    assert_file_unchanged(&p1, &b1);
    assert_file_unchanged(&p2, &b2);
}

#[test]
fn t1_f23_03_shared_asset_deduplicated_in_imagens_and_manifest() {
    let env = TestEnv::new();
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Asset Compartilhado", &["A", "B"]);
    let img = fixture_path("images/joao_silva.jpg");
    env.services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &img)
        .unwrap();
    env.services
        .photo_service
        .link_manual(&pid, &recs[1].id, &v_foto.id, &img)
        .unwrap();

    let out_dir = env.path().join("pkg_dedup");
    let summary = env
        .services
        .export_service
        .export_package(&pid, &out_dir, false)
        .unwrap();
    assert_eq!(summary.exported_assets, 1);

    let img_entries: Vec<_> = std::fs::read_dir(out_dir.join("imagens"))
        .unwrap()
        .collect();
    assert_eq!(img_entries.len(), 1);
    assert!(out_dir.join("imagens/asset_001.jpg").exists());

    let xml = std::fs::read_to_string(out_dir.join("dados.xml")).unwrap();
    let datasets = extract_xml_datasets(&xml);
    assert_eq!(datasets.len(), 2);
    assert_eq!(datasets[0].1.get("Foto").map(String::as_str), Some("imagens/asset_001.jpg"));
    assert_eq!(datasets[1].1.get("Foto").map(String::as_str), Some("imagens/asset_001.jpg"));

    let manifest = read_manifest_json(&out_dir);
    assert!(manifest.to_string().contains("asset_001.jpg"));
}

#[test]
fn t1_f23_04_manifest_json_and_relatorio_csv_accuracy() {
    let env = TestEnv::new();
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Manifest e CSV", &["Ana", "Bruno"]);
    env.services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();
    env.services
        .photo_service
        .link_manual(&pid, &recs[1].id, &v_foto.id, &fixture_path("images/carlos_eduardo.png"))
        .unwrap();

    let out_dir = env.path().join("pkg_meta");
    env.services
        .export_service
        .export_package(&pid, &out_dir, false)
        .unwrap();

    let manifest = read_manifest_json(&out_dir);
    assert_eq!(manifest["dataset_count"].as_u64(), Some(2));
    assert!(manifest["path_strategy"].as_str().is_some());
    let mappings = manifest["asset_mappings"].as_array().expect("asset_mappings array");
    assert_eq!(mappings.len(), 2);
    assert!(mappings[0]["source_path"].as_str().is_some());
    assert_eq!(mappings[0]["exported_rel_path"].as_str(), Some("imagens/asset_001.jpg"));
    assert_eq!(mappings[0]["size_bytes"].as_u64(), Some(683));
    assert_eq!(mappings[1]["exported_rel_path"].as_str(), Some("imagens/asset_002.png"));
    assert_eq!(mappings[1]["size_bytes"].as_u64(), Some(134));

    let (_headers, rows) = read_relatorio_csv(&out_dir);
    assert_eq!(rows.len(), 2);
    assert!(rows[0].iter().any(|c| c == "DS_001"));
    assert!(rows[1].iter().any(|c| c == "DS_002"));
}

#[test]
fn t1_f23_05_overwrite_confirmed_true_replaces_existing_cleanly() {
    let env = TestEnv::new();
    let (pid, vn, v_foto, recs) = setup_photo_project(&env, "Sobrescrita", &["Inicial"]);
    env.services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    let out_dir = env.path().join("pkg_overwrite");
    env.services
        .export_service
        .export_package(&pid, &out_dir, false)
        .unwrap();

    env.services
        .data_service
        .update_cell(&pid, &recs[0].id, &vn.id, "Atualizado")
        .unwrap();
    env.services
        .export_service
        .export_package(&pid, &out_dir, true)
        .unwrap();

    let xml = std::fs::read_to_string(out_dir.join("dados.xml")).unwrap();
    assert!(xml.contains("Atualizado"));
    for dir_to_check in [&out_dir, out_dir.parent().unwrap()] {
        for entry in std::fs::read_dir(dir_to_check).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            assert!(
                !name.starts_with(".aivm_stage_"),
                "leftover staging dir {name}"
            );
        }
    }
}

// ============================================================================
// F-24: Path Prefix / Portability Updater (5 tests)
// ============================================================================

#[test]
fn t1_f24_01_move_folder_and_update_absolute_file_uri_prefix() {
    let env = TestEnv::new();
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Mover Pacote", &["Pessoa 1"]);
    env.services
        .project_service
        .set_path_strategy(&pid, PathStrategy::AbsoluteFileUri)
        .unwrap();
    env.services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    let dir_a = env.path().join("dir_a");
    let dir_b = env.path().join("dir_b");
    env.services
        .export_service
        .export_package(&pid, &dir_a, false)
        .unwrap();
    std::fs::rename(&dir_a, &dir_b).unwrap();

    env.services
        .export_service
        .update_exported_prefix(&dir_b, PathStrategy::AbsoluteFileUri)
        .unwrap();

    let xml = std::fs::read_to_string(dir_b.join("dados.xml")).unwrap();
    let datasets = extract_xml_datasets(&xml);
    let uri = datasets[0].1.get("Foto").unwrap();
    assert!(uri.starts_with("file:///"));
    assert!(uri.contains("dir_b"));
    assert!(!uri.contains("dir_a"));
    assert!(uri.ends_with("/imagens/asset_001.jpg"));
    assert_illustrator_xml_valid(&xml, 1);

    let manifest = read_manifest_json(&dir_b);
    assert_eq!(manifest["path_strategy"].as_str(), Some("absolute_file_uri"));
    assert!(!manifest.to_string().contains("dir_a"));
}

#[test]
fn t1_f24_02_switch_strategy_absolute_to_relative() {
    let env = TestEnv::new();
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Abs para Rel", &["Pessoa 1"]);
    env.services
        .project_service
        .set_path_strategy(&pid, PathStrategy::AbsoluteFileUri)
        .unwrap();
    env.services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    let out_dir = env.path().join("switch_rel");
    env.services
        .export_service
        .export_package(&pid, &out_dir, false)
        .unwrap();
    env.services
        .export_service
        .update_exported_prefix(&out_dir, PathStrategy::RelativeToXml)
        .unwrap();

    let xml = std::fs::read_to_string(out_dir.join("dados.xml")).unwrap();
    assert!(xml.contains("<Foto>imagens/asset_001.jpg</Foto>"));
    let manifest = read_manifest_json(&out_dir);
    assert_eq!(manifest["path_strategy"].as_str(), Some("relative_to_xml"));
}

#[test]
fn t1_f24_03_switch_strategy_relative_to_absolute() {
    let env = TestEnv::new();
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Rel para Abs", &["Pessoa 1"]);
    env.services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    let out_dir = env.path().join("switch_abs");
    env.services
        .export_service
        .export_package(&pid, &out_dir, false)
        .unwrap();
    env.services
        .export_service
        .update_exported_prefix(&out_dir, PathStrategy::AbsoluteFileUri)
        .unwrap();

    let xml = std::fs::read_to_string(out_dir.join("dados.xml")).unwrap();
    let datasets = extract_xml_datasets(&xml);
    let uri = datasets[0].1.get("Foto").unwrap();
    assert!(uri.starts_with("file:///"));
    assert!(uri.ends_with("/imagens/asset_001.jpg"));
    env.services
        .export_service
        .validate_xml_structure(&xml, 1)
        .unwrap();
    assert_illustrator_xml_valid(&xml, 1);
}

#[test]
fn t1_f24_04_preserves_xml_headers_text_values_and_entities() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Preservar XML Prefix", ProjectMode::WithTemplate)
        .unwrap();
    env.services
        .export_service
        .bind_xml_template(
            &proj.id,
            Some(&fixture_path("template_mixed.xml")),
            &read_fixture_string("template_mixed.xml"),
        )
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let _v_func = env
        .services
        .project_service
        .create_variable(&proj.id, "Funcao", Some("Funcao"), VariableKind::Text, false)
        .unwrap();
    let _v_mat = env
        .services
        .project_service
        .create_variable(&proj.id, "Matricula", Some("Matricula"), VariableKind::Text, false)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&proj.id, "Foto", Some("Foto"), VariableKind::LinkedFile, false)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01 - Ana & B".into(), vec![(&v_nome, "Ana & <Silva>")])],
    );
    env.services
        .photo_service
        .link_manual(&proj.id, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    let out_dir = env.path().join("preserve_prefix");
    env.services
        .export_service
        .export_package(&proj.id, &out_dir, false)
        .unwrap();
    let xml_before = std::fs::read_to_string(out_dir.join("dados.xml")).unwrap();

    env.services
        .export_service
        .update_exported_prefix(&out_dir, PathStrategy::AbsoluteFileUri)
        .unwrap();
    let xml_after = std::fs::read_to_string(out_dir.join("dados.xml")).unwrap();

    let before_datasets_start = xml_before.find("<v:sampleDataSets").unwrap();
    let after_datasets_start = xml_after.find("<v:sampleDataSets").unwrap();
    assert_eq!(
        &xml_before[..before_datasets_start],
        &xml_after[..after_datasets_start]
    );

    let ds_before = extract_xml_datasets(&xml_before);
    let ds_after = extract_xml_datasets(&xml_after);
    assert_eq!(ds_before[0].0, ds_after[0].0);
    assert_eq!(ds_before[0].1.get("Nome"), ds_after[0].1.get("Nome"));
    assert_ne!(ds_before[0].1.get("Foto"), ds_after[0].1.get("Foto"));
}

#[test]
fn t1_f24_05_returns_accurate_prefix_update_report() {
    let env = TestEnv::new();
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Report Prefix", &["A", "B", "C"]);
    let img1 = fixture_path("images/joao_silva.jpg");
    let img2 = fixture_path("images/carlos_eduardo.png");
    env.services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &img1)
        .unwrap();
    env.services
        .photo_service
        .link_manual(&pid, &recs[1].id, &v_foto.id, &img2)
        .unwrap();
    env.services
        .photo_service
        .link_manual(&pid, &recs[2].id, &v_foto.id, &img1)
        .unwrap();

    let out_dir = env.path().join("report_prefix");
    env.services
        .export_service
        .export_package(&pid, &out_dir, false)
        .unwrap();

    let rep = env
        .services
        .export_service
        .update_exported_prefix(&out_dir, PathStrategy::AbsoluteFileUri)
        .unwrap();
    assert_eq!(rep.updated_references, 3);
    assert_eq!(rep.verified_assets, 2);
    assert!(rep.missing_assets.is_empty());
    assert_eq!(rep.previous_strategy, PathStrategy::RelativeToXml);
    assert_eq!(rep.new_strategy, PathStrategy::AbsoluteFileUri);
    assert_eq!(rep.updated_dados_xml, out_dir.join("dados.xml"));
    assert_eq!(rep.updated_manifest_json, out_dir.join("manifest.json"));
}
