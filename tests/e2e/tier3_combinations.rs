use ai_variable_manager::domain::{
    CellDiagnosticKind, ImportColumnMapping, ImportColumnTarget, ImportMode, MatchStatus,
    NewVariableSpec, ProjectMode, ThemePreference, ThumbnailState, ValidationCode,
    ValidationTarget, Variable, VariableKind,
};

use super::helpers::{
    MINIMAL_JPEG_2X2, TestEnv, assert_illustrator_xml_matches_columns,
    assert_illustrator_xml_valid, assert_utf8_no_bom_crlf, extract_xml_datasets, fixture_path,
    generate_synthetic_photo_dir, read_fixture_string, read_manifest_json,
    request_thumbnail_blocking, seed_records_and_values,
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

#[test]
fn t3_01_sqlite_and_project_lifecycle_shared_state() {
    let env = TestEnv::new();
    let p1 = env
        .services
        .project_service
        .create_project("Projeto Um", ProjectMode::WithoutTemplate)
        .unwrap();
    let p2 = env
        .services
        .project_service
        .create_project("Projeto Dois", ProjectMode::WithTemplate)
        .unwrap();
    env.services
        .project_service
        .create_variable(&p2.id, "Col", Some("Col"), VariableKind::Text, true)
        .unwrap();

    env.services
        .project_service
        .set_theme_preference(ThemePreference::Dark)
        .unwrap();
    env.services
        .project_service
        .rename_project(&p1.id, "Projeto Um Renomeado")
        .unwrap();
    env.services.project_service.delete_project(&p2.id).unwrap();

    let reopened = env.reopen();
    assert_eq!(
        reopened.project_service.get_theme_preference().unwrap(),
        ThemePreference::Dark
    );
    let list = reopened.project_service.list_projects().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, p1.id);
    assert_eq!(list[0].name, "Projeto Um Renomeado");
    assert!(reopened.project_service.open_project(&p2.id).is_err());
}

#[test]
fn t3_02_project_and_dynamic_columns_data_flow() {
    let env = TestEnv::new();
    let p = env
        .services
        .project_service
        .create_project("Com Modelo", ProjectMode::WithTemplate)
        .unwrap();
    let v1 = env
        .services
        .project_service
        .create_variable(&p.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let v2 = env
        .services
        .project_service
        .create_variable(&p.id, "Cargo", Some("Cargo"), VariableKind::Text, false)
        .unwrap();
    let v3 = env
        .services
        .project_service
        .create_variable(&p.id, "Setor", Some("Setor"), VariableKind::Text, false)
        .unwrap();
    let v4 = env
        .services
        .project_service
        .create_variable(&p.id, "Foto", Some("Foto"), VariableKind::LinkedFile, true)
        .unwrap();

    env.services
        .project_service
        .rename_variable(&p.id, &v2.id, "Função", Some("Funcao"))
        .unwrap();
    env.services
        .project_service
        .reorder_variables(
            &p.id,
            &[v4.id.clone(), v1.id.clone(), v3.id.clone(), v2.id.clone()],
        )
        .unwrap();

    let reopened = env.reopen();
    let snap = reopened.project_service.open_project(&p.id).unwrap();
    let names: Vec<(&str, usize)> = snap
        .variables
        .iter()
        .map(|v| (v.xml_name.as_str(), v.sort_order))
        .collect();
    assert_eq!(
        names,
        vec![("Foto", 0), ("Nome", 1), ("Setor", 2), ("Funcao", 3)]
    );
}

#[test]
fn t3_03_virtualized_grid_and_block_paste_reorder() {
    let env = TestEnv::new();
    let p = env
        .services
        .project_service
        .create_project("Grade + Colagem", ProjectMode::WithoutTemplate)
        .unwrap();
    let v1 = env
        .services
        .project_service
        .create_variable(&p.id, "Col1", Some("Col1"), VariableKind::Text, true)
        .unwrap();
    let v2 = env
        .services
        .project_service
        .create_variable(&p.id, "Col2", Some("Col2"), VariableKind::Text, false)
        .unwrap();

    let mut initial_rows = Vec::with_capacity(200);
    for i in 0..200 {
        initial_rows.push((format!("DS_{i:03}"), vec![(&v1, "Init"), (&v2, "Base")]));
    }
    seed_records_and_values(&env.services, &p.id, &initial_rows);

    let mut tsv = String::new();
    for i in 0..25 {
        tsv.push_str(&format!("Pasted_{i:02}\tDept_{i:02}\r\n"));
    }
    let pasted = env
        .services
        .data_service
        .paste_block(&p.id, 50, 0, &tsv)
        .unwrap();
    assert_eq!(pasted, 25);

    let snap = env.services.project_service.open_project(&p.id).unwrap();
    assert_eq!(snap.records.len(), 200);
    let r74_id = snap.records[74].id.clone();

    let mut new_order: Vec<String> = snap.records.iter().map(|r| r.id.clone()).collect();
    let moved = new_order.remove(74);
    new_order.insert(0, moved);
    env.services
        .data_service
        .reorder_records(&p.id, &new_order)
        .unwrap();

    let w_top = env
        .services
        .data_service
        .get_records_window(&p.id, 0, 10)
        .unwrap();
    assert_eq!(w_top.records[0].id, r74_id);
    let val_top = w_top
        .values
        .iter()
        .find(|v| v.record_id == r74_id && v.variable_id == v1.id)
        .unwrap();
    assert_eq!(val_top.text_value, "Pasted_24");

    let w_mid = env
        .services
        .data_service
        .get_records_window(&p.id, 51, 5)
        .unwrap();
    assert_eq!(w_mid.records.len(), 5);
}

#[test]
fn t3_04_paste_and_dynamic_column_duplicate_delete() {
    let env = TestEnv::new();
    let p = env
        .services
        .project_service
        .create_project("Colar + Colunas", ProjectMode::WithoutTemplate)
        .unwrap();
    let v0 = env
        .services
        .project_service
        .create_variable(&p.id, "C0", Some("C0"), VariableKind::Text, false)
        .unwrap();
    let v1 = env
        .services
        .project_service
        .create_variable(&p.id, "C1", Some("C1"), VariableKind::Text, false)
        .unwrap();
    let v2 = env
        .services
        .project_service
        .create_variable(&p.id, "C2", Some("C2"), VariableKind::Text, false)
        .unwrap();

    env.services
        .data_service
        .paste_block(&p.id, 0, 0, "A0\tA1\tA2\nB0\tB1\tB2\nC0\tC1\tC2\nD0\tD1\tD2\n")
        .unwrap();

    let dup = env
        .services
        .project_service
        .duplicate_variable(&p.id, &v1.id)
        .unwrap();
    env.services
        .project_service
        .delete_variable(&p.id, &v0.id)
        .unwrap();

    let snap = env.services.project_service.open_project(&p.id).unwrap();
    env.services
        .data_service
        .update_cell(&p.id, &snap.records[0].id, &dup.id, "A1_Edit")
        .unwrap();

    let after = env.services.project_service.open_project(&p.id).unwrap();
    assert_eq!(after.variables.len(), 3);
    assert_eq!(after.value_for(&snap.records[0].id, &dup.id).unwrap().text_value, "A1_Edit");
    assert_eq!(after.value_for(&snap.records[1].id, &dup.id).unwrap().text_value, "B1");
    assert_eq!(after.value_for(&snap.records[0].id, &v2.id).unwrap().text_value, "A2");
}

#[test]
fn t3_05_column_xml_name_auto_suggest_duplicate_and_required() {
    let env = TestEnv::new();
    let p = env
        .services
        .project_service
        .create_project("NCName + Colunas", ProjectMode::WithoutTemplate)
        .unwrap();
    let v = env
        .services
        .project_service
        .create_variable(&p.id, "1ª Função", None, VariableKind::Text, false)
        .unwrap();
    assert!(v.xml_name.starts_with("var_1"));

    let dup = env
        .services
        .project_service
        .duplicate_variable(&p.id, &v.id)
        .unwrap();
    assert_ne!(dup.xml_name, v.xml_name);
    assert!(dup.xml_name.contains("_copia"));

    env.services
        .project_service
        .update_variable_options(&p.id, &dup.id, VariableKind::Text, true, None)
        .unwrap();

    let snap = env.services.project_service.open_project(&p.id).unwrap();
    assert!(snap.variable_by_id(&dup.id).unwrap().required);
}

#[test]
fn t3_06_xlsx_import_with_mapping_zeros_and_cell_flags() {
    let env = TestEnv::new();
    let p = env
        .services
        .project_service
        .create_project("XLSX + Flags", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_cod = env
        .services
        .project_service
        .create_variable(&p.id, "CodExist", Some("CodExist"), VariableKind::Text, true)
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
            target: ImportColumnTarget::NewVariable(new_var_spec("Funcao", VariableKind::Text, false)),
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
            &p.id,
            &fixture_path("sample_badges.xlsx"),
            Some("Crachas"),
            0,
            &mappings,
            ImportMode::Append,
        )
        .unwrap();
    assert_eq!(n, 5);

    let snap = env.services.project_service.open_project(&p.id).unwrap();
    assert_eq!(snap.value_for(&snap.records[0].id, &v_cod.id).unwrap().text_value, "00421");
    assert_eq!(snap.value_for(&snap.records[1].id, &v_cod.id).unwrap().text_value, "00007");

    let diags = env.services.data_service.compute_cell_diagnostics(&p.id).unwrap();
    assert!(diags.iter().any(|d| d.kind == CellDiagnosticKind::DuplicateValue && d.variable_id == v_cod.id));
    assert!(diags.iter().any(|d| d.kind == CellDiagnosticKind::EmptyOptional));
}

#[test]
fn t3_07_csv_win1252_and_utf8_bom_mapping_and_flags() {
    let env = TestEnv::new();
    let p = env
        .services
        .project_service
        .create_project("CSVs + Flags", ProjectMode::WithoutTemplate)
        .unwrap();

    let m_win = vec![
        ImportColumnMapping {
            source_col_index: 0,
            target: ImportColumnTarget::NewVariable(new_var_spec("Codigo", VariableKind::Text, true)),
        },
        ImportColumnMapping {
            source_col_index: 1,
            target: ImportColumnTarget::NewVariable(new_var_spec("Nome", VariableKind::Text, true)),
        },
    ];
    let n1 = env
        .services
        .import_service
        .commit_import(
            &p.id,
            &fixture_path("sample_win1252_semicolon.csv"),
            None,
            0,
            &m_win,
            ImportMode::Append,
        )
        .unwrap();
    assert_eq!(n1, 3);

    let snap1 = env.services.project_service.open_project(&p.id).unwrap();
    let v_cod = snap1.variables[0].id.clone();
    let v_nome = snap1.variables[1].id.clone();
    assert_eq!(snap1.value_for(&snap1.records[0].id, &v_nome).unwrap().text_value, "João Carlos");

    let m_bom = vec![
        ImportColumnMapping {
            source_col_index: 0,
            target: ImportColumnTarget::ExistingVariable(v_cod.clone()),
        },
        ImportColumnMapping {
            source_col_index: 1,
            target: ImportColumnTarget::ExistingVariable(v_nome.clone()),
        },
    ];
    let n2 = env
        .services
        .import_service
        .commit_import(
            &p.id,
            &fixture_path("sample_utf8_bom_tab.csv"),
            None,
            0,
            &m_bom,
            ImportMode::Append,
        )
        .unwrap();
    assert_eq!(n2, 4);

    let snap2 = env.services.project_service.open_project(&p.id).unwrap();
    assert_eq!(snap2.records.len(), 7);
    let diags = env.services.data_service.compute_cell_diagnostics(&p.id).unwrap();
    assert!(diags.iter().any(|d| d.variable_id == v_cod && d.kind == CellDiagnosticKind::DuplicateValue));
}

#[test]
fn t3_08_manual_cell_fix_clears_empty_and_duplicate_diagnostics() {
    let env = TestEnv::new();
    let p = env
        .services
        .project_service
        .create_project("Corrigir Flags", ProjectMode::WithoutTemplate)
        .unwrap();
    let m = vec![
        ImportColumnMapping {
            source_col_index: 0,
            target: ImportColumnTarget::NewVariable(new_var_spec("codigo", VariableKind::Text, true)),
        },
        ImportColumnMapping {
            source_col_index: 2,
            target: ImportColumnTarget::NewVariable(new_var_spec("funcao", VariableKind::Text, false)),
        },
    ];
    env.services
        .import_service
        .commit_import(
            &p.id,
            &fixture_path("sample_utf8_comma.csv"),
            None,
            0,
            &m,
            ImportMode::Append,
        )
        .unwrap();

    let snap = env.services.project_service.open_project(&p.id).unwrap();
    let v_cod = &snap.variables[0];
    let v_func = &snap.variables[1];

    // Row 3 duplicates codigo "00421"; Row 2 has empty funcao
    let diags_before = env.services.data_service.compute_cell_diagnostics(&p.id).unwrap();
    assert!(diags_before.iter().any(|d| d.record_id == snap.records[3].id && d.kind == CellDiagnosticKind::DuplicateValue));
    assert!(diags_before.iter().any(|d| d.record_id == snap.records[2].id && d.kind == CellDiagnosticKind::EmptyOptional));

    env.services
        .data_service
        .update_cell(&p.id, &snap.records[3].id, &v_cod.id, "00777")
        .unwrap();
    env.services
        .data_service
        .update_cell(&p.id, &snap.records[2].id, &v_func.id, "Designer")
        .unwrap();

    let diags_after = env.services.data_service.compute_cell_diagnostics(&p.id).unwrap();
    assert!(!diags_after.iter().any(|d| d.variable_id == v_cod.id && d.kind == CellDiagnosticKind::DuplicateValue));
    assert!(!diags_after.iter().any(|d| d.record_id == snap.records[2].id && d.variable_id == v_func.id));
}

#[test]
fn t3_09_scan_220_photos_and_bounded_thumbnail_cache() {
    let env = TestEnv::new();
    let dir = env.path().join("scan_220");
    generate_synthetic_photo_dir(&dir, 220);
    for i in 0..3 {
        std::fs::copy(
            fixture_path("images/ignored_sample.heic"),
            dir.join(format!("ignored_{i}.heic")),
        )
        .unwrap();
    }

    let p = env
        .services
        .project_service
        .create_project("Scan 220 + LRU", ProjectMode::WithoutTemplate)
        .unwrap();
    let scan = env.services.photo_service.scan_directory(&p.id, &dir).unwrap();
    assert_eq!(scan.assets.len(), 220);
    assert_eq!(scan.skipped_unsupported, 3);

    let snap = env.services.project_service.open_project(&p.id).unwrap();
    for asset in &snap.assets {
        let state = request_thumbnail_blocking(&env.services, &asset.id, &asset.source_path).unwrap();
        assert!(matches!(state, ThumbnailState::Ready(_)));
        assert!(env.services.photo_service.thumbnail_cache_len().unwrap() <= 192);
    }
    assert_eq!(env.services.photo_service.thumbnail_cache_len().unwrap(), 192);
}

#[test]
fn t3_10_scan_mixed_extensions_and_auto_match_preserves_text() {
    let env = TestEnv::new();
    let p = env
        .services
        .project_service
        .create_project("Scan + Match", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&p.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&p.id, "Foto", Some("Foto"), VariableKind::LinkedFile, false)
        .unwrap();

    let recs = seed_records_and_values(
        &env.services,
        &p.id,
        &[
            ("01".into(), vec![(&v_nome, "  marcela goncalves  "), (&v_foto, "  marcela goncalves  ")]),
            ("02".into(), vec![(&v_nome, "uppercase_ext"), (&v_foto, "uppercase_ext")]),
            ("03".into(), vec![(&v_nome, "nested_foto"), (&v_foto, "nested_foto")]),
        ],
    );
    env.services
        .photo_service
        .scan_directory(&p.id, &fixture_path("images"))
        .unwrap();
    let diag = env.services.photo_service.run_auto_match(&p.id).unwrap();
    assert_eq!(diag.matched_count, 3);

    let snap = env.services.project_service.open_project(&p.id).unwrap();
    assert_eq!(
        snap.value_for(&recs[0].id, &v_nome.id).unwrap().text_value,
        "  marcela goncalves  "
    );
    assert_eq!(
        snap.value_for(&recs[0].id, &v_foto.id).unwrap().match_status,
        MatchStatus::AutoExact
    );
}

#[test]
fn t3_11_auto_match_tie_and_fuzzy_resolved_via_manual_link() {
    let env = TestEnv::new();
    let photos_dir = env.copy_fixtures_images_to("t3_11_photos");
    std::fs::write(photos_dir.join("Roberto Henrrique Martins.jpg"), MINIMAL_JPEG_2X2).unwrap();

    let p = env
        .services
        .project_service
        .create_project("Auto + Manual", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&p.id, "Foto", Some("Foto"), VariableKind::LinkedFile, true)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &p.id,
        &[
            ("01".into(), vec![(&v_foto, "Ana Silva")]),
            ("02".into(), vec![(&v_foto, "Roberto Henrique Martins")]),
        ],
    );
    env.services
        .photo_service
        .scan_directory(&p.id, &photos_dir)
        .unwrap();
    let diag = env.services.photo_service.run_auto_match(&p.id).unwrap();
    assert_eq!(diag.ambiguous_count, 1);
    assert_eq!(diag.suggested_count, 1);

    env.services
        .photo_service
        .link_manual(&p.id, &recs[0].id, &v_foto.id, &photos_dir.join("Ana Silva.png"))
        .unwrap();
    env.services
        .photo_service
        .link_manual(
            &p.id,
            &recs[1].id,
            &v_foto.id,
            &photos_dir.join("Roberto Henrrique Martins.jpg"),
        )
        .unwrap();

    let snap = env.services.project_service.open_project(&p.id).unwrap();
    assert_eq!(snap.value_for(&recs[0].id, &v_foto.id).unwrap().match_status, MatchStatus::Manual);
    assert_eq!(snap.value_for(&recs[1].id, &v_foto.id).unwrap().match_status, MatchStatus::Manual);
    assert!(snap.match_candidates.is_empty());
}

#[test]
fn t3_12_auto_match_plus_batch_unmatched_completes_all() {
    let env = TestEnv::new();
    let photos_dir = env.path().join("t3_12_photos");
    generate_synthetic_photo_dir(&photos_dir, 7);

    let p = env
        .services
        .project_service
        .create_project("Auto + Batch", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&p.id, "Foto", Some("Foto"), VariableKind::LinkedFile, true)
        .unwrap();
    seed_records_and_values(
        &env.services,
        &p.id,
        &[
            ("01".into(), vec![(&v_foto, "Pessoa_0001")]),
            ("02".into(), vec![(&v_foto, "Pessoa_0002")]),
            ("03".into(), vec![(&v_foto, "Pessoa_0003")]),
            ("04".into(), vec![(&v_foto, "Pessoa_0004")]),
            ("05".into(), vec![(&v_foto, "Sem_A")]),
            ("06".into(), vec![(&v_foto, "Sem_B")]),
            ("07".into(), vec![(&v_foto, "Sem_C")]),
        ],
    );

    env.services
        .photo_service
        .scan_directory(&p.id, &photos_dir)
        .unwrap();
    env.services.photo_service.run_auto_match(&p.id).unwrap();

    let pool = env
        .services
        .photo_service
        .get_unmatched_pool(&p.id, &v_foto.id)
        .unwrap();
    assert_eq!(pool.records.len(), 3);
    assert_eq!(pool.assets.len(), 3);

    let pairs: Vec<(String, String, String)> = pool
        .records
        .iter()
        .zip(pool.assets.iter())
        .map(|(r, a)| (r.id.clone(), v_foto.id.clone(), a.id.clone()))
        .collect();
    let res = env
        .services
        .photo_service
        .batch_link(&p.id, &pairs, false)
        .unwrap();
    assert_eq!(res.linked_count, 3);

    let pool_after = env
        .services
        .photo_service
        .get_unmatched_pool(&p.id, &v_foto.id)
        .unwrap();
    assert!(pool_after.records.is_empty());
}

#[test]
fn t3_13_manual_duplicate_and_deleted_disk_file_diagnostics() {
    let env = TestEnv::new();
    let photos_dir = env.copy_fixtures_images_to("t3_13_photos");
    let shared = photos_dir.join("joao_silva.jpg");
    let doomed = photos_dir.join("carlos_eduardo.png");

    let p = env
        .services
        .project_service
        .create_project("Manual + Diag", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&p.id, "Foto", Some("Foto"), VariableKind::LinkedFile, false)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &p.id,
        &[("01".into(), vec![]), ("02".into(), vec![]), ("03".into(), vec![])],
    );

    env.services
        .photo_service
        .link_manual(&p.id, &recs[0].id, &v_foto.id, &shared)
        .unwrap();
    env.services
        .photo_service
        .link_manual(&p.id, &recs[1].id, &v_foto.id, &shared)
        .unwrap();
    env.services
        .photo_service
        .link_manual(&p.id, &recs[2].id, &v_foto.id, &doomed)
        .unwrap();

    std::fs::remove_file(&doomed).unwrap();
    let diag = env
        .services
        .photo_service
        .compute_diagnostics(&p.id, true)
        .unwrap();
    assert_eq!(diag.duplicate_asset_assignments.len(), 1);
    assert_eq!(diag.missing_on_disk_asset_ids.len(), 1);
}

#[test]
fn t3_14_batch_unlink_and_disk_delete_updates_diagnostics() {
    let env = TestEnv::new();
    let photos_dir = env.path().join("t3_14_photos");
    generate_synthetic_photo_dir(&photos_dir, 3);

    let p = env
        .services
        .project_service
        .create_project("Batch + Diag", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&p.id, "Foto", Some("Foto"), VariableKind::LinkedFile, false)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &p.id,
        &[("01".into(), vec![]), ("02".into(), vec![]), ("03".into(), vec![])],
    );

    env.services
        .photo_service
        .scan_directory(&p.id, &photos_dir)
        .unwrap();
    let pool = env
        .services
        .photo_service
        .get_unmatched_pool(&p.id, &v_foto.id)
        .unwrap();
    let pairs: Vec<(String, String, String)> = recs
        .iter()
        .zip(pool.assets.iter())
        .map(|(r, a)| (r.id.clone(), v_foto.id.clone(), a.id.clone()))
        .collect();
    env.services.photo_service.batch_link(&p.id, &pairs, false).unwrap();

    env.services
        .photo_service
        .unlink_photo(&p.id, &recs[0].id, &v_foto.id)
        .unwrap();
    std::fs::remove_file(&pool.assets[1].source_path).unwrap();

    let diag = env
        .services
        .photo_service
        .compute_diagnostics(&p.id, true)
        .unwrap();
    assert_eq!(diag.records_missing_photo, vec![recs[0].id.clone()]);
    assert_eq!(diag.unmatched_asset_ids, vec![pool.assets[0].id.clone()]);
    assert_eq!(diag.missing_on_disk_asset_ids, vec![pool.assets[1].id.clone()]);
}

#[test]
fn t3_15_missing_disk_photo_blocks_with_e06_then_restores_export() {
    let env = TestEnv::new();
    let photos_dir = env.copy_fixtures_images_to("t3_15_photos");
    let target = photos_dir.join("joao_silva.jpg");

    let p = env
        .services
        .project_service
        .create_project("E06 Ciclo", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&p.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&p.id, "Foto", Some("Foto"), VariableKind::LinkedFile, true)
        .unwrap();
    let recs = seed_records_and_values(&env.services, &p.id, &[("01".into(), vec![(&v_nome, "Joao")])]);
    env.services
        .photo_service
        .link_manual(&p.id, &recs[0].id, &v_foto.id, &target)
        .unwrap();

    std::fs::remove_file(&target).unwrap();
    let audit1 = env.services.validation_service.audit_project(&p.id).unwrap();
    assert!(!audit1.can_export);
    assert!(audit1.issues.iter().any(|i| i.code == ValidationCode::E06LinkedAssetMissingOnDisk));

    std::fs::write(&target, MINIMAL_JPEG_2X2).unwrap();
    let diag = env.services.photo_service.compute_diagnostics(&p.id, true).unwrap();
    assert!(diag.missing_on_disk_asset_ids.is_empty());

    let audit2 = env.services.validation_service.audit_project(&p.id).unwrap();
    assert!(audit2.can_export);
    env.services
        .export_service
        .export_package(&p.id, &env.path().join("out_restored"), false)
        .unwrap();
}

#[test]
fn t3_16_template_xml_name_validation_blocks_e02() {
    let env = TestEnv::new();
    let p = env
        .services
        .project_service
        .create_project("Template + E02", ProjectMode::WithTemplate)
        .unwrap();
    let parsed = env
        .services
        .export_service
        .bind_xml_template(
            &p.id,
            Some(&fixture_path("template_mixed.xml")),
            &read_fixture_string("template_mixed.xml"),
        )
        .unwrap();
    assert_eq!(parsed.variables.len(), 4);

    let bad_var = Variable {
        id: "v-bad-ncname".into(),
        project_id: p.id.clone(),
        display_name: "Ruim".into(),
        xml_name: "9Bad:Name".into(),
        kind: VariableKind::Text,
        required: false,
        sort_order: 0,
        mapped_template_var: Some("Nome".into()),
    };
    env.services.project_repo.upsert_variable(&bad_var).unwrap();
    seed_records_and_values(&env.services, &p.id, &[("01".into(), vec![(&bad_var, "A")])]);

    let audit = env.services.validation_service.audit_project(&p.id).unwrap();
    assert!(!audit.can_export);
    assert!(audit.issues.iter().any(|i| i.code == ValidationCode::E02InvalidOrDuplicateXmlName));
}

#[test]
fn t3_17_c2pa_template_with_special_xml_chars_escapes_cleanly() {
    let env = TestEnv::new();
    let p = env
        .services
        .project_service
        .create_project("C2PA + Escape", ProjectMode::WithTemplate)
        .unwrap();
    let tpl_raw = read_fixture_string("segunda_leva_variant.xml");
    env.services
        .export_service
        .bind_xml_template(&p.id, Some(&fixture_path("segunda_leva_variant.xml")), &tpl_raw)
        .unwrap();

    let v_nome = env
        .services
        .project_service
        .create_variable(&p.id, "NomeFuncionario", Some("NomeFuncionario"), VariableKind::Text, true)
        .unwrap();
    let v_func = env
        .services
        .project_service
        .create_variable(&p.id, "FuncaoFuncionario", Some("FuncaoFuncionario"), VariableKind::Text, false)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&p.id, "FotoFuncionario", Some("FotoFuncionario"), VariableKind::LinkedFile, false)
        .unwrap();

    let ds_name = "01 - P&D <TI> \"A\" 'B'";
    let nome_val = "João & Maria <1> \"2\" '3'";
    let recs = seed_records_and_values(
        &env.services,
        &p.id,
        &[(ds_name.into(), vec![(&v_nome, nome_val), (&v_func, "Chefe & Lider")])],
    );
    env.services
        .photo_service
        .link_manual(&p.id, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    let out_dir = env.path().join("c2pa_escaped");
    env.services
        .export_service
        .export_package(&p.id, &out_dir, false)
        .unwrap();
    let xml_bytes = std::fs::read(out_dir.join("dados.xml")).unwrap();
    assert_utf8_no_bom_crlf(&xml_bytes);
    let xml_str = String::from_utf8(xml_bytes).unwrap();
    assert!(xml_str.contains("<c2pa:manifest>AAAWgmp1bWJHZW5lcmF0ZWRTeW50aGV0aWNDMlBBTWFuaWZlc3REYXRh</c2pa:manifest>"));
    assert_illustrator_xml_valid(&xml_str, 1);

    let datasets = extract_xml_datasets(&xml_str);
    assert_eq!(datasets[0].0, ds_name);
    assert_eq!(datasets[0].1.get("NomeFuncionario").map(String::as_str), Some(nome_val));
}

#[test]
fn t3_18_without_template_csv_then_late_bind_bombeiro_legacy_template() {
    let env = TestEnv::new();
    let p = env
        .services
        .project_service
        .create_project("Bombeiro Tardio", ProjectMode::WithoutTemplate)
        .unwrap();

    let mappings = vec![
        ImportColumnMapping {
            source_col_index: 1,
            target: ImportColumnTarget::NewVariable(new_var_spec("NomeCol", VariableKind::Text, true)),
        },
        ImportColumnMapping {
            source_col_index: 2,
            target: ImportColumnTarget::NewVariable(new_var_spec("FuncaoCol", VariableKind::Text, false)),
        },
        ImportColumnMapping {
            source_col_index: 3,
            target: ImportColumnTarget::NewVariable(new_var_spec("FotoCol", VariableKind::LinkedFile, false)),
        },
    ];
    env.services
        .import_service
        .commit_import(
            &p.id,
            &fixture_path("sample_bombeiros_semicolon.csv"),
            None,
            0,
            &mappings,
            ImportMode::Append,
        )
        .unwrap();

    env.services
        .export_service
        .bind_xml_template(
            &p.id,
            Some(&fixture_path("bombeiro_reference.xml")),
            &read_fixture_string("bombeiro_reference.xml"),
        )
        .unwrap();

    let snap = env.services.project_service.open_project(&p.id).unwrap();
    env.services
        .project_service
        .update_variable_options(&p.id, &snap.variables[0].id, VariableKind::Text, true, Some("nome".into()))
        .unwrap();
    env.services
        .project_service
        .update_variable_options(&p.id, &snap.variables[1].id, VariableKind::Text, false, Some("funcao".into()))
        .unwrap();
    env.services
        .project_service
        .update_variable_options(&p.id, &snap.variables[2].id, VariableKind::LinkedFile, false, Some("foto".into()))
        .unwrap();

    env.services
        .photo_service
        .link_manual(
            &p.id,
            &snap.records[0].id,
            &snap.variables[2].id,
            &fixture_path("images/joao_silva.jpg"),
        )
        .unwrap();

    let out_dir = env.path().join("bombeiro_late");
    env.services
        .export_service
        .export_package(&p.id, &out_dir, false)
        .unwrap();
    let xml = std::fs::read_to_string(out_dir.join("dados.xml")).unwrap();
    assert_illustrator_xml_matches_columns(&xml, 6, &["nome", "funcao"], &["foto"]);
}

#[test]
fn t3_19_export_twice_relative_and_absolute_uri_strategies() {
    let env = TestEnv::new();
    let p = env
        .services
        .project_service
        .create_project("Duas Estratégias", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&p.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&p.id, "Foto", Some("Foto"), VariableKind::LinkedFile, true)
        .unwrap();
    let recs = seed_records_and_values(&env.services, &p.id, &[("01".into(), vec![(&v_nome, "Ana")])]);
    env.services
        .photo_service
        .link_manual(&p.id, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    let out_a = env.path().join("out_a");
    env.services
        .export_service
        .export_package(&p.id, &out_a, false)
        .unwrap();
    let xml_a = std::fs::read_to_string(out_a.join("dados.xml")).unwrap();
    assert!(xml_a.contains(&format!("<Foto>{}\\imagens\\asset_001.jpg</Foto>", out_a.display())));
    assert_eq!(read_manifest_json(&out_a)["path_strategy"].as_str(), Some("absolute_path"));

    let out_b = env.path().join("out_b com espaço");
    env.services
        .export_service
        .export_package(&p.id, &out_b, false)
        .unwrap();
    let xml_b = std::fs::read_to_string(out_b.join("dados.xml")).unwrap();
    let ds = extract_xml_datasets(&xml_b);
    assert_eq!(
        ds[0].1.get("Foto").map(String::as_str),
        Some(format!("{}\\imagens\\asset_001.jpg", out_b.display()).as_str())
    );
    assert_eq!(read_manifest_json(&out_b)["path_strategy"].as_str(), Some("absolute_path"));
}

#[test]
fn t3_20_audit_e05_blocks_export_until_photo_linked_with_w01_warning() {
    let env = TestEnv::new();
    let p = env
        .services
        .project_service
        .create_project("Gate E05", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&p.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let v_obs = env
        .services
        .project_service
        .create_variable(&p.id, "Obs", Some("Obs"), VariableKind::Text, false)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&p.id, "Foto", Some("Foto"), VariableKind::LinkedFile, true)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &p.id,
        &[("01".into(), vec![(&v_nome, "Carlos"), (&v_obs, "")])],
    );

    let out_dir = env.path().join("gated_out");
    assert!(env.services.export_service.export_package(&p.id, &out_dir, false).is_err());

    env.services
        .photo_service
        .link_manual(&p.id, &recs[0].id, &v_foto.id, &fixture_path("images/carlos_eduardo.png"))
        .unwrap();
    let audit = env.services.validation_service.audit_project(&p.id).unwrap();
    assert!(audit.can_export);
    assert_eq!(audit.warning_count, 1);

    env.services
        .export_service
        .export_package(&p.id, &out_dir, false)
        .unwrap();
    assert!(out_dir.join("dados.xml").exists());
}

#[test]
fn t3_21_export_move_with_spaces_and_update_prefix() {
    let env = TestEnv::new();
    let p = env
        .services
        .project_service
        .create_project("Mover + Prefixo", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&p.id, "Foto", Some("Foto"), VariableKind::LinkedFile, true)
        .unwrap();
    let recs = seed_records_and_values(&env.services, &p.id, &[("01".into(), vec![])]);
    env.services
        .photo_service
        .link_manual(&p.id, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    let v1_dir = env.path().join("v1");
    let v2_dir = env.path().join("v2 com espaço");
    env.services
        .export_service
        .export_package(&p.id, &v1_dir, false)
        .unwrap();
    std::fs::rename(&v1_dir, &v2_dir).unwrap();

    env.services
        .export_service
        .update_exported_prefix(&v2_dir)
        .unwrap();

    let xml = std::fs::read_to_string(v2_dir.join("dados.xml")).unwrap();
    assert_illustrator_xml_valid(&xml, 1);
    let ds = extract_xml_datasets(&xml);
    let path_val = ds[0].1.get("Foto").unwrap();
    assert!(path_val.contains(r"v2 com espaço\imagens\asset_001.jpg"));
    assert!(!path_val.starts_with("file://"));
    assert!(v2_dir.join("imagens/asset_001.jpg").is_file());
    assert!(read_manifest_json(&v2_dir)["export_dir"].as_str().unwrap().contains("v2 com espaço"));
}

#[test]
fn t3_22_cached_template_survives_disk_deletion_across_reopen() {
    let env = TestEnv::new();
    let tpl_copy = env.copy_fixture_file("template_mixed.xml", "temp_tpl.xml");
    let tpl_raw = std::fs::read_to_string(&tpl_copy).unwrap();

    let p = env
        .services
        .project_service
        .create_project("Cache Offline", ProjectMode::WithTemplate)
        .unwrap();
    env.services
        .export_service
        .bind_xml_template(&p.id, Some(&tpl_copy), &tpl_raw)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&p.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let v_func = env
        .services
        .project_service
        .create_variable(&p.id, "Funcao", Some("Funcao"), VariableKind::Text, false)
        .unwrap();
    let v_mat = env
        .services
        .project_service
        .create_variable(&p.id, "Matricula", Some("Matricula"), VariableKind::Text, false)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&p.id, "Foto", Some("Foto"), VariableKind::LinkedFile, false)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &p.id,
        &[("01".into(), vec![(&v_nome, "Ana"), (&v_func, "Dev"), (&v_mat, "001")])],
    );
    env.services
        .photo_service
        .link_manual(&p.id, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    std::fs::remove_file(&tpl_copy).unwrap();
    let reopened = env.reopen();
    let out_dir = env.path().join("out_cached_tpl");
    reopened
        .export_service
        .export_package(&p.id, &out_dir, false)
        .unwrap();
    assert_illustrator_xml_valid(&std::fs::read_to_string(out_dir.join("dados.xml")).unwrap(), 1);
}

#[test]
fn t3_23_csv_leading_zeros_and_auto_match_by_matricula() {
    let env = TestEnv::new();
    let csv_path = env.path().join("mat_match.csv");
    std::fs::write(
        &csv_path,
        "Matricula;Nome;Foto\r\n00421;José da Silva;00421\r\n",
    )
    .unwrap();
    let photos_dir = env.path().join("mat_photos");
    std::fs::create_dir_all(&photos_dir).unwrap();
    std::fs::write(photos_dir.join("00421.jpg"), MINIMAL_JPEG_2X2).unwrap();

    let p = env
        .services
        .project_service
        .create_project("Zeros + Match", ProjectMode::WithoutTemplate)
        .unwrap();
    let mappings = vec![
        ImportColumnMapping {
            source_col_index: 0,
            target: ImportColumnTarget::NewVariable(new_var_spec("Matricula", VariableKind::Text, true)),
        },
        ImportColumnMapping {
            source_col_index: 1,
            target: ImportColumnTarget::NewVariable(new_var_spec("Nome", VariableKind::Text, true)),
        },
        ImportColumnMapping {
            source_col_index: 2,
            target: ImportColumnTarget::NewVariable(new_var_spec("Foto", VariableKind::LinkedFile, true)),
        },
    ];
    env.services
        .import_service
        .commit_import(&p.id, &csv_path, None, 0, &mappings, ImportMode::Append)
        .unwrap();
    env.services
        .photo_service
        .scan_directory(&p.id, &photos_dir)
        .unwrap();
    let diag = env.services.photo_service.run_auto_match(&p.id).unwrap();
    assert_eq!(diag.matched_count, 1);

    let snap = env.services.project_service.open_project(&p.id).unwrap();
    assert_eq!(
        snap.value_for(&snap.records[0].id, &snap.variables[0].id)
            .unwrap()
            .text_value,
        "00421"
    );
}

#[test]
fn t3_24_duplicate_required_column_flags_e04_on_both_distinct_variables() {
    let env = TestEnv::new();
    let p = env
        .services
        .project_service
        .create_project("Dup Required", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_orig = env
        .services
        .project_service
        .create_variable(&p.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let recs = seed_records_and_values(&env.services, &p.id, &[("01".into(), vec![(&v_orig, "")])]);

    let v_dup = env
        .services
        .project_service
        .duplicate_variable(&p.id, &v_orig.id)
        .unwrap();
    env.services
        .project_service
        .reorder_variables(&p.id, &[v_dup.id.clone(), v_orig.id.clone()])
        .unwrap();

    let audit = env.services.validation_service.audit_project(&p.id).unwrap();
    assert!(!audit.can_export);
    assert!(audit.issues.iter().any(|i| {
        i.code == ValidationCode::E04RequiredTextEmpty
            && i.target
                == ValidationTarget::Cell {
                    record_id: recs[0].id.clone(),
                    variable_id: v_orig.id.clone(),
                }
    }));
    assert!(audit.issues.iter().any(|i| {
        i.code == ValidationCode::E04RequiredTextEmpty
            && i.target
                == ValidationTarget::Cell {
                    record_id: recs[0].id.clone(),
                    variable_id: v_dup.id.clone(),
                }
    }));
}
