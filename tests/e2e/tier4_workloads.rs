use ai_variable_manager::domain::{
    ImportColumnMapping, ImportColumnTarget, ImportMode, NewVariableSpec,
    ProjectMode, ThumbnailState, ValidationCode, VariableKind,
};
use ai_variable_manager::shared::error::{AppError, DomainError};

use super::helpers::{
    MINIMAL_JPEG_2X2, TestEnv, assert_file_unchanged, assert_illustrator_xml_matches_columns,
    assert_illustrator_xml_valid, assert_utf8_no_bom_crlf, extract_xml_datasets, fixture_path,
    generate_synthetic_photo_dir, read_fixture_string, read_manifest_json, read_relatorio_csv,
    request_thumbnail_blocking, seed_records_and_values, snapshot_bytes,
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
fn t4_01_secom_badges_full_workflow_with_template() {
    let env = TestEnv::new();
    let tpl_raw = read_fixture_string("secom_reference.xml");
    let p = env
        .services
        .project_service
        .create_project("SECOM 2026", ProjectMode::WithTemplate)
        .unwrap();
    env.services
        .export_service
        .bind_xml_template(&p.id, Some(&fixture_path("secom_reference.xml")), &tpl_raw)
        .unwrap();

    let v_nome = env
        .services
        .project_service
        .create_variable(&p.id, "NomeFuncionario", Some("NomeFuncionario"), VariableKind::Text, true)
        .unwrap();
    let v_func = env
        .services
        .project_service
        .create_variable(&p.id, "FuncaoFuncionario", Some("FuncaoFuncionario"), VariableKind::Text, true)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&p.id, "FotoFuncionario", Some("FotoFuncionario"), VariableKind::LinkedFile, true)
        .unwrap();

    let photos_dir = env.path().join("secom_photos");
    std::fs::create_dir_all(&photos_dir).unwrap();
    let mut rows = Vec::new();
    let mut photo_snapshots = Vec::new();
    for i in 1..=10 {
        let name = format!("95 - João Carlos Silva Morais {i:02}");
        let photo_path = photos_dir.join(format!("95 - Joao Carlos Silva Morais {i:02}.jpg"));
        std::fs::write(&photo_path, MINIMAL_JPEG_2X2).unwrap();
        photo_snapshots.push((photo_path.clone(), snapshot_bytes(&photo_path)));
        rows.push((
            format!("0{i:02} - Servidor {i}"),
            vec![(&v_nome, "João Carlos Silva"), (&v_func, "Assessor Técnico"), (&v_foto, "")],
        ));
        let _ = name;
    }
    let recs = seed_records_and_values(&env.services, &p.id, &rows);
    for (idx, rec) in recs.iter().enumerate() {
        let lookup = format!("95 - João Carlos Silva Morais {:02}", idx + 1);
        env.services
            .data_service
            .update_cell(&p.id, &rec.id, &v_foto.id, &lookup)
            .unwrap();
    }

    env.services.photo_service.scan_directory(&p.id, &photos_dir).unwrap();
    let diag = env.services.photo_service.run_auto_match(&p.id).unwrap();
    assert_eq!(diag.matched_count, 10);

    let audit = env.services.validation_service.audit_project(&p.id).unwrap();
    assert!(audit.can_export);
    assert_eq!(audit.blocking_count, 0);

    let out_dir = env.path().join("Exportacao_SECOM");
    let summary = env
        .services
        .export_service
        .export_package(&p.id, &out_dir, false)
        .unwrap();
    assert_eq!(summary.exported_datasets, 10);
    assert_eq!(summary.exported_assets, 10);

    let xml_bytes = snapshot_bytes(&out_dir.join("dados.xml"));
    assert_utf8_no_bom_crlf(&xml_bytes);
    let xml_str = String::from_utf8(xml_bytes).unwrap();
    assert_illustrator_xml_valid(&xml_str, 10);

    let tpl_start = tpl_raw.find("<v:sampleDataSets").unwrap();
    let gen_start = xml_str.find("<v:sampleDataSets").unwrap();
    assert_eq!(&xml_str[..gen_start], &tpl_raw[..tpl_start]);
    assert!(xml_str.contains(&format!("<FotoFuncionario>{}\\imagens\\asset_001.jpg</FotoFuncionario>", out_dir.display())));
    assert!(xml_str.contains(&format!("<FotoFuncionario>{}\\imagens\\asset_010.jpg</FotoFuncionario>", out_dir.display())));

    let manifest = read_manifest_json(&out_dir);
    assert_eq!(manifest["dataset_count"].as_u64(), Some(10));
    let (_hdr, csv_rows) = read_relatorio_csv(&out_dir);
    assert_eq!(csv_rows.len(), 10);

    for (path, before) in &photo_snapshots {
        assert_file_unchanged(path, before);
    }
}

#[test]
fn t4_02_bombeiros_semicolon_csv_tie_resolution_and_absolute_uri() {
    let env = TestEnv::new();
    let tpl_raw = read_fixture_string("bombeiro_reference.xml");
    let p = env
        .services
        .project_service
        .create_project("Bombeiros", ProjectMode::WithTemplate)
        .unwrap();
    env.services
        .export_service
        .bind_xml_template(&p.id, Some(&fixture_path("bombeiro_reference.xml")), &tpl_raw)
        .unwrap();

    let mappings = vec![
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

    let photos_dir = env.copy_fixtures_images_to("bombeiro_fotos");
    let snap = env.services.project_service.open_project(&p.id).unwrap();
    let v_foto = snap.variables.iter().find(|v| v.xml_name == "foto").unwrap();
    // Row 0 = Ana Silva (triggers same-stem .jpg vs .png tie!)
    env.services
        .data_service
        .update_cell(&p.id, &snap.records[0].id, &v_foto.id, "Ana Silva")
        .unwrap();
    env.services.photo_service.scan_directory(&p.id, &photos_dir).unwrap();
    let diag = env.services.photo_service.run_auto_match(&p.id).unwrap();
    assert_eq!(diag.ambiguous_count, 1);

    let audit_blocked = env.services.validation_service.audit_project(&p.id).unwrap();
    assert!(!audit_blocked.can_export);
    assert!(audit_blocked.issues.iter().any(|i| i.code == ValidationCode::E10UnresolvedAmbiguousPhoto));

    env.services
        .photo_service
        .link_manual(&p.id, &snap.records[0].id, &v_foto.id, &photos_dir.join("Ana Silva.png"))
        .unwrap();
    let out_dir = env.path().join("Bombeiros_Out");
    env.services
        .export_service
        .export_package(&p.id, &out_dir, false)
        .unwrap();
    let xml = std::fs::read_to_string(out_dir.join("dados.xml")).unwrap();
    assert_illustrator_xml_matches_columns(&xml, 6, &["nome", "funcao"], &["foto"]);
    let datasets = extract_xml_datasets(&xml);
    let path_val = datasets[0].1.get("foto").unwrap();
    assert!(!path_val.starts_with("file://"));
    assert!(path_val.ends_with(r"\imagens\asset_001.png"));
}

#[test]
fn t4_03_segunda_leva_c2pa_win1252_csv_and_fuzzy_diacritics() {
    let env = TestEnv::new();
    let tpl_raw = read_fixture_string("segunda_leva_variant.xml");
    let p = env
        .services
        .project_service
        .create_project("Segunda Leva", ProjectMode::WithTemplate)
        .unwrap();
    env.services
        .export_service
        .bind_xml_template(&p.id, Some(&fixture_path("segunda_leva_variant.xml")), &tpl_raw)
        .unwrap();

    let mappings = vec![
        ImportColumnMapping {
            source_col_index: 1,
            target: ImportColumnTarget::NewVariable(new_var_spec("NomeFuncionario", VariableKind::Text, true)),
        },
        ImportColumnMapping {
            source_col_index: 2,
            target: ImportColumnTarget::NewVariable(new_var_spec("FuncaoFuncionario", VariableKind::Text, false)),
        },
    ];
    env.services
        .import_service
        .commit_import(
            &p.id,
            &fixture_path("sample_win1252_semicolon.csv"),
            None,
            0,
            &mappings,
            ImportMode::Append,
        )
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&p.id, "FotoFuncionario", Some("FotoFuncionario"), VariableKind::LinkedFile, false)
        .unwrap();

    let photos_dir = env.path().join("segunda_leva_fotos");
    std::fs::create_dir_all(&photos_dir).unwrap();
    std::fs::write(photos_dir.join("JOAO  CARLOS.jpg"), MINIMAL_JPEG_2X2).unwrap();
    std::fs::write(photos_dir.join("MARIA CONCEICAO.png"), super::helpers::MINIMAL_PNG_2X2).unwrap();
    std::fs::write(photos_dir.join("PEDRO HENRRIQUE.jpg"), MINIMAL_JPEG_2X2).unwrap();

    let snap = env.services.project_service.open_project(&p.id).unwrap();
    env.services
        .data_service
        .update_cell(&p.id, &snap.records[0].id, &v_foto.id, "João Carlos")
        .unwrap();
    env.services
        .data_service
        .update_cell(&p.id, &snap.records[1].id, &v_foto.id, "Maria Conceição")
        .unwrap();
    env.services
        .data_service
        .update_cell(&p.id, &snap.records[2].id, &v_foto.id, "Pedro Henrique")
        .unwrap();

    env.services.photo_service.scan_directory(&p.id, &photos_dir).unwrap();
    let diag = env.services.photo_service.run_auto_match(&p.id).unwrap();
    assert_eq!(diag.matched_count, 2);
    assert_eq!(diag.suggested_count, 1);

    let snap_mid = env.services.project_service.open_project(&p.id).unwrap();
    let cand = snap_mid
        .match_candidates
        .iter()
        .find(|c| c.record_id == snap.records[2].id)
        .unwrap()
        .clone();
    env.services
        .photo_service
        .accept_match_candidate(&p.id, &cand.record_id, &cand.variable_id, &cand.asset_id)
        .unwrap();

    let out_dir = env.path().join("SegundaLeva_Out");
    let summary = env.services.export_service.export_package(&p.id, &out_dir, false).unwrap();
    assert_eq!(summary.exported_assets, 3);
    let xml = std::fs::read_to_string(out_dir.join("dados.xml")).unwrap();
    assert!(xml.contains("<c2pa:manifest>AAAWgmp1bWJHZW5lcmF0ZWRTeW50aGV0aWNDMlBBTWFuaWZlc3REYXRh</c2pa:manifest>"));
    assert!(xml.contains("<variableSets  xmlns=\"&ns_vars;\">"));
    assert_illustrator_xml_valid(&xml, 3);
}

#[test]
fn t4_04_without_template_paste_batch_link_then_late_bind() {
    let env = TestEnv::new();
    let photos_dir = env.path().join("t4_04_fotos");
    generate_synthetic_photo_dir(&photos_dir, 3);

    let p = env
        .services
        .project_service
        .create_project("Preparação Livre", ProjectMode::WithoutTemplate)
        .unwrap();
    let v1 = env
        .services
        .project_service
        .create_variable(&p.id, "ColNome", Some("ColNome"), VariableKind::Text, true)
        .unwrap();
    let v2 = env
        .services
        .project_service
        .create_variable(&p.id, "ColFunc", Some("ColFunc"), VariableKind::Text, false)
        .unwrap();
    let v3 = env
        .services
        .project_service
        .create_variable(&p.id, "ColMat", Some("ColMat"), VariableKind::Text, false)
        .unwrap();
    let v4 = env
        .services
        .project_service
        .create_variable(&p.id, "ColFoto", Some("ColFoto"), VariableKind::LinkedFile, false)
        .unwrap();

    env.services
        .data_service
        .paste_block(
            &p.id,
            0,
            0,
            "Ana\tDesigner\t001\r\nBruno\tDev\t002\r\nCarla\tQA\t003\r\n",
        )
        .unwrap();

    env.services.photo_service.scan_directory(&p.id, &photos_dir).unwrap();
    let pool = env.services.photo_service.get_unmatched_pool(&p.id, &v4.id).unwrap();
    let pairs: Vec<_> = pool
        .records
        .iter()
        .zip(pool.assets.iter())
        .map(|(r, a)| (r.id.clone(), v4.id.clone(), a.id.clone()))
        .collect();
    env.services.photo_service.batch_link(&p.id, &pairs, false).unwrap();

    let tpl_raw = read_fixture_string("template_mixed.xml");
    env.services
        .export_service
        .bind_xml_template(&p.id, Some(&fixture_path("template_mixed.xml")), &tpl_raw)
        .unwrap();
    env.services.project_service.update_variable_options(&p.id, &v1.id, VariableKind::Text, true, Some("Nome".into())).unwrap();
    env.services.project_service.update_variable_options(&p.id, &v2.id, VariableKind::Text, false, Some("Funcao".into())).unwrap();
    env.services.project_service.update_variable_options(&p.id, &v3.id, VariableKind::Text, false, Some("Matricula".into())).unwrap();
    env.services.project_service.update_variable_options(&p.id, &v4.id, VariableKind::LinkedFile, false, Some("Foto".into())).unwrap();

    let out_dir = env.path().join("t4_04_out");
    env.services.export_service.export_package(&p.id, &out_dir, false).unwrap();
    let xml = std::fs::read_to_string(out_dir.join("dados.xml")).unwrap();
    assert_illustrator_xml_valid(&xml, 3);
    assert!(xml.contains("<Nome><p>Ana</p></Nome>"));
}

#[test]
fn t4_05_direct_envelope_export_move_and_prefix_roundtrip() {
    let env = TestEnv::new();
    let photos_dir = env.path().join("t4_05_fotos");
    generate_synthetic_photo_dir(&photos_dir, 5);

    let p = env
        .services
        .project_service
        .create_project("Direto + Mover", ProjectMode::WithoutTemplate)
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
    let recs = seed_records_and_values(
        &env.services,
        &p.id,
        &[
            ("01".into(), vec![(&v_nome, "P1"), (&v_foto, "Pessoa_0001")]),
            ("02".into(), vec![(&v_nome, "P2"), (&v_foto, "Pessoa_0002")]),
            ("03".into(), vec![(&v_nome, "P3"), (&v_foto, "Pessoa_0003")]),
            ("04".into(), vec![(&v_nome, "P4"), (&v_foto, "Pessoa_0004")]),
            ("05".into(), vec![(&v_nome, "P5"), (&v_foto, "Pessoa_0005")]),
        ],
    );
    let _ = recs;
    env.services.photo_service.scan_directory(&p.id, &photos_dir).unwrap();
    env.services.photo_service.run_auto_match(&p.id).unwrap();

    let dir_1 = env.path().join("dir_1");
    let dir_2 = env.path().join("dir_2").join("Sub Pasta (Final)");
    std::fs::create_dir_all(dir_2.parent().unwrap()).unwrap();

    env.services.export_service.export_package(&p.id, &dir_1, false).unwrap();
    std::fs::rename(&dir_1, &dir_2).unwrap();

    let rep_abs = env
        .services
        .export_service
        .update_exported_prefix(&dir_2)
        .unwrap();
    assert_eq!(rep_abs.updated_references, 5);
    assert_eq!(rep_abs.verified_assets, 5);
    let xml_abs = std::fs::read_to_string(dir_2.join("dados.xml")).unwrap();
    assert_illustrator_xml_valid(&xml_abs, 5);
    assert!(extract_xml_datasets(&xml_abs)[0].1.get("Foto").unwrap().contains(r"Sub Pasta (Final)\imagens\asset_001.jpg"));
    assert!(!xml_abs.contains("file://"));
}

#[test]
fn t4_06_multi_sheet_xlsx_zeros_paste_diagnostics_and_export() {
    let env = TestEnv::new();
    let xlsx_path = fixture_path("sample_badges.xlsx");
    let sheets = env.services.import_service.list_sheets(&xlsx_path).unwrap();
    assert_eq!(sheets, vec!["Crachas".to_string(), "ListaExtra".to_string()]);

    let p = env
        .services
        .project_service
        .create_project("XLSX Completo", ProjectMode::WithoutTemplate)
        .unwrap();
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
            target: ImportColumnTarget::Ignore,
        },
    ];
    let n = env
        .services
        .import_service
        .commit_import(&p.id, &xlsx_path, Some("ListaExtra"), 1, &mappings, ImportMode::Append)
        .unwrap();
    assert_eq!(n, 3);

    env.services
        .data_service
        .paste_block(&p.id, 3, 0, "00421\tExtra Um\tAnalista\r\n00421\tExtra Dois\t\r\n")
        .unwrap();

    let audit = env.services.validation_service.audit_project(&p.id).unwrap();
    assert!(audit.can_export);
    assert!(audit.issues.iter().any(|i| i.code == ValidationCode::W01OptionalCellEmpty));
    assert!(audit.issues.iter().any(|i| i.code == ValidationCode::W03DuplicateKeyTextValue));

    let out_dir = env.path().join("xlsx_out");
    env.services.export_service.export_package(&p.id, &out_dir, false).unwrap();
    let xml = std::fs::read_to_string(out_dir.join("dados.xml")).unwrap();
    assert!(xml.contains("<matricula><p>00888</p></matricula>"));
    assert!(xml.contains("<matricula><p>00001</p></matricula>"));
    assert!(xml.contains("<matricula><p>00421</p></matricula>"));
}

#[test]
fn t4_07_mixed_media_directory_full_3_mode_photo_workflow() {
    let env = TestEnv::new();
    let photos_dir = fixture_path("images");
    let p = env
        .services
        .project_service
        .create_project("3 Modos Fotos", ProjectMode::WithoutTemplate)
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
    let recs = seed_records_and_values(
        &env.services,
        &p.id,
        &[
            ("01".into(), vec![(&v_nome, "João"), (&v_foto, "joao_silva")]),
            ("02".into(), vec![(&v_nome, "Águia"), (&v_foto, "manual_tiff")]),
            ("03".into(), vec![(&v_nome, "Upper"), (&v_foto, "manual_png")]),
            ("04".into(), vec![(&v_nome, "Nested"), (&v_foto, "lote_nested")]),
        ],
    );

    let scan = env.services.photo_service.scan_directory(&p.id, &photos_dir).unwrap();
    assert_eq!(scan.skipped_unsupported, 2);

    // Mode A
    env.services.photo_service.run_auto_match(&p.id).unwrap();
    // Mode B
    env.services
        .photo_service
        .link_manual(&p.id, &recs[1].id, &v_foto.id, &photos_dir.join("aguia_dourada.tiff"))
        .unwrap();
    env.services
        .photo_service
        .link_manual(&p.id, &recs[2].id, &v_foto.id, &photos_dir.join("UPPERCASE_EXT.PNG"))
        .unwrap();
    // Mode C
    let pool = env.services.photo_service.get_unmatched_pool(&p.id, &v_foto.id).unwrap();
    let nested_asset = pool
        .assets
        .iter()
        .find(|a| a.file_name == "nested_foto.tif")
        .unwrap();
    env.services
        .photo_service
        .batch_link(
            &p.id,
            &[(recs[3].id.clone(), v_foto.id.clone(), nested_asset.id.clone())],
            false,
        )
        .unwrap();

    let snap = env.services.project_service.open_project(&p.id).unwrap();
    for a in &snap.assets {
        let state = request_thumbnail_blocking(&env.services, &a.id, &a.source_path).unwrap();
        assert!(matches!(state, ThumbnailState::Ready(_)));
    }

    let out_dir = env.path().join("media_3_modes");
    let summary = env.services.export_service.export_package(&p.id, &out_dir, false).unwrap();
    assert_eq!(summary.exported_assets, 4);
    assert!(out_dir.join("imagens/asset_001.jpg").is_file());
    assert!(out_dir.join("imagens/asset_002.tiff").is_file());
    assert!(out_dir.join("imagens/asset_003.png").is_file());
    assert!(out_dir.join("imagens/asset_004.tif").is_file());
}

#[test]
fn t4_08_pre_export_blocking_error_remediation_loop() {
    let env = TestEnv::new();
    let photos_dir = env.copy_fixtures_images_to("t4_08_fotos");
    let doomed = photos_dir.join("joao_silva.jpg");

    let p = env
        .services
        .project_service
        .create_project("Remediação", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_req = env
        .services
        .project_service
        .create_variable(&p.id, "Nome", Some("Nome"), VariableKind::Text, true)
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
            ("DUP".into(), vec![(&v_req, ""), (&v_foto, "Ana Silva")]),
            ("DUP".into(), vec![(&v_req, "Bruno"), (&v_foto, "")]),
            ("OK_3".into(), vec![(&v_req, "Carlos"), (&v_foto, "")]),
        ],
    );
    env.services.photo_service.scan_directory(&p.id, &photos_dir).unwrap();
    env.services.photo_service.run_auto_match(&p.id).unwrap();
    env.services
        .photo_service
        .link_manual(&p.id, &recs[2].id, &v_foto.id, &doomed)
        .unwrap();
    std::fs::remove_file(&doomed).unwrap();

    let out_dir = env.path().join("remediation_out");
    let blocked = env.services.validation_service.audit_project(&p.id).unwrap();
    assert!(!blocked.can_export);
    assert!(blocked.issues.iter().any(|i| i.code == ValidationCode::E03DuplicateDatasetName));
    assert!(blocked.issues.iter().any(|i| i.code == ValidationCode::E04RequiredTextEmpty));
    assert!(blocked.issues.iter().any(|i| i.code == ValidationCode::E05RequiredPhotoMissing));
    assert!(blocked.issues.iter().any(|i| i.code == ValidationCode::E06LinkedAssetMissingOnDisk));
    assert!(blocked.issues.iter().any(|i| i.code == ValidationCode::E10UnresolvedAmbiguousPhoto));
    assert!(env.services.export_service.export_package(&p.id, &out_dir, false).is_err());
    assert!(!out_dir.join("dados.xml").exists());

    // Step-by-step fix
    env.services.data_service.update_dataset_name(&p.id, &recs[0].id, "DS_01").unwrap();
    env.services.data_service.update_dataset_name(&p.id, &recs[1].id, "DS_02").unwrap();
    env.services.data_service.update_cell(&p.id, &recs[0].id, &v_req.id, "Ana").unwrap();
    env.services
        .photo_service
        .link_manual(&p.id, &recs[0].id, &v_foto.id, &photos_dir.join("Ana Silva.jpg"))
        .unwrap();
    env.services
        .photo_service
        .link_manual(&p.id, &recs[1].id, &v_foto.id, &photos_dir.join("carlos_eduardo.png"))
        .unwrap();
    std::fs::write(&doomed, MINIMAL_JPEG_2X2).unwrap();
    env.services.photo_service.compute_diagnostics(&p.id, true).unwrap();

    let clean = env.services.validation_service.audit_project(&p.id).unwrap();
    assert!(clean.can_export);
    env.services.export_service.export_package(&p.id, &out_dir, false).unwrap();
    assert!(out_dir.join("dados.xml").is_file());
}

#[test]
fn t4_09_overwrite_guard_and_clean_reexport() {
    let env = TestEnv::new();
    let p = env
        .services
        .project_service
        .create_project("Reexport", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&p.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    seed_records_and_values(
        &env.services,
        &p.id,
        &[
            ("01".into(), vec![(&v_nome, "A")]),
            ("02".into(), vec![(&v_nome, "B")]),
            ("03".into(), vec![(&v_nome, "C")]),
        ],
    );

    let out_dir = env.path().join("reexport_pkg");
    env.services.export_service.export_package(&p.id, &out_dir, false).unwrap();
    let batch1_bytes = snapshot_bytes(&out_dir.join("dados.xml"));

    for i in 4..=5 {
        let r = env.services.data_service.add_record(&p.id, Some(&format!("0{i}"))).unwrap();
        env.services.data_service.update_cell(&p.id, &r.id, &v_nome.id, &format!("Item {i}")).unwrap();
    }

    let err = env
        .services
        .export_service
        .export_package(&p.id, &out_dir, false)
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::Domain(DomainError::OverwriteConfirmationRequired { .. })
    ));
    assert_file_unchanged(&out_dir.join("dados.xml"), &batch1_bytes);

    env.services.export_service.export_package(&p.id, &out_dir, true).unwrap();
    let xml2 = std::fs::read_to_string(out_dir.join("dados.xml")).unwrap();
    assert_illustrator_xml_valid(&xml2, 5);
    assert_eq!(read_manifest_json(&out_dir)["dataset_count"].as_u64(), Some(5));
}

#[test]
fn t4_10_multi_session_sqlite_reopen_and_offline_template_resilience() {
    let env = TestEnv::new();

    // Session 1: create project + CSV import
    let pid = {
        let p = env
            .services
            .project_service
            .create_project("Multi Sessão", ProjectMode::WithoutTemplate)
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
                target: ImportColumnTarget::NewVariable(new_var_spec("Funcao", VariableKind::Text, false)),
            },
            ImportColumnMapping {
                source_col_index: 4,
                target: ImportColumnTarget::NewVariable(new_var_spec("Foto", VariableKind::LinkedFile, false)),
            },
        ];
        env.services
            .import_service
            .commit_import(
                &p.id,
                &fixture_path("sample_utf8_comma.csv"),
                None,
                0,
                &mappings,
                ImportMode::Append,
            )
            .unwrap();
        p.id
    };

    // Session 2: reopen, bind temp XML template, then delete temp XML file from disk
    let tpl_copy = env.copy_fixture_file("template_mixed.xml", "ephemeral_tpl.xml");
    {
        let s2 = env.reopen();
        let tpl_raw = std::fs::read_to_string(&tpl_copy).unwrap();
        s2.export_service.bind_xml_template(&pid, Some(&tpl_copy), &tpl_raw).unwrap();
        s2.photo_service.scan_directory(&pid, &fixture_path("images")).unwrap();
        s2.photo_service.run_auto_match(&pid).unwrap();
    }
    std::fs::remove_file(&tpl_copy).unwrap();

    // Session 3: reopen, reorder records, rename column display_name, export
    let out_dir = env.path().join("multisess_out");
    {
        let s3 = env.reopen();
        let snap = s3.project_service.open_project(&pid).unwrap();
        s3.project_service
            .rename_variable(&pid, &snap.variables[0].id, "Matrícula Atualizada", None)
            .unwrap();
        let mut ids: Vec<String> = snap.records.iter().map(|r| r.id.clone()).collect();
        ids.reverse();
        s3.data_service.reorder_records(&pid, &ids).unwrap();
        s3.export_service.export_package(&pid, &out_dir, false).unwrap();
    }

    // Session 4: reopen & run prefix update
    {
        let s4 = env.reopen();
        let _ = s4.project_service.open_project(&pid).unwrap();
        s4.export_service
            .update_exported_prefix(&out_dir)
            .unwrap();
    }

    let xml = std::fs::read_to_string(out_dir.join("dados.xml")).unwrap();
    assert_illustrator_xml_valid(&xml, 6);
}

#[test]
fn t4_11_volume_1000_records_and_1000_photos_bounded_memory() {
    let env = TestEnv::new();
    let photos_dir = env.path().join("vol_1000_photos");
    let photo_paths = generate_synthetic_photo_dir(&photos_dir, 1000);
    let sample_photo_before = snapshot_bytes(&photo_paths[0]);
    let sample_photo_last_before = snapshot_bytes(&photo_paths[999]);

    let csv_path = env.path().join("vol_1000.csv");
    let mut csv_buf = String::from("Codigo;Nome;Cargo;Setor;Obs;Foto\r\n");
    for i in 1..=1000 {
        csv_buf.push_str(&format!(
            "{i:05};Servidor {i:04};Cargo {i};Setor {};Obs {i};Pessoa_{i:04}\r\n",
            i % 10
        ));
    }
    std::fs::write(&csv_path, csv_buf).unwrap();

    let p = env
        .services
        .project_service
        .create_project("Volume 1000x1000", ProjectMode::WithoutTemplate)
        .unwrap();
    let mappings = vec![
        ImportColumnMapping {
            source_col_index: 0,
            target: ImportColumnTarget::NewVariable(new_var_spec("Codigo", VariableKind::Text, true)),
        },
        ImportColumnMapping {
            source_col_index: 1,
            target: ImportColumnTarget::NewVariable(new_var_spec("Nome", VariableKind::Text, true)),
        },
        ImportColumnMapping {
            source_col_index: 2,
            target: ImportColumnTarget::NewVariable(new_var_spec("Cargo", VariableKind::Text, true)),
        },
        ImportColumnMapping {
            source_col_index: 3,
            target: ImportColumnTarget::NewVariable(new_var_spec("Setor", VariableKind::Text, true)),
        },
        ImportColumnMapping {
            source_col_index: 4,
            target: ImportColumnTarget::NewVariable(new_var_spec("Obs", VariableKind::Text, false)),
        },
        ImportColumnMapping {
            source_col_index: 5,
            target: ImportColumnTarget::NewVariable(new_var_spec("Foto", VariableKind::LinkedFile, true)),
        },
    ];
    let imported = env
        .services
        .import_service
        .commit_import(&p.id, &csv_path, None, 0, &mappings, ImportMode::Append)
        .unwrap();
    assert_eq!(imported, 1000);

    let scan = env.services.photo_service.scan_directory(&p.id, &photos_dir).unwrap();
    assert_eq!(scan.assets.len(), 1000);

    let diag = env.services.photo_service.run_auto_match(&p.id).unwrap();
    assert_eq!(diag.matched_count, 1000);

    let snap = env.services.project_service.open_project(&p.id).unwrap();
    for asset in &snap.assets {
        let state = request_thumbnail_blocking(&env.services, &asset.id, &asset.source_path).unwrap();
        assert!(matches!(state, ThumbnailState::Ready(_)));
        assert!(env.services.photo_service.thumbnail_cache_len().unwrap() <= 192);
    }

    let audit = env.services.validation_service.audit_project(&p.id).unwrap();
    assert!(audit.can_export);
    assert_eq!(audit.blocking_count, 0);

    let out_dir = env.path().join("vol_1000_out");
    let summary = env.services.export_service.export_package(&p.id, &out_dir, false).unwrap();
    assert_eq!(summary.exported_datasets, 1000);
    assert_eq!(summary.exported_assets, 1000);
    assert!(out_dir.join("imagens/asset_001.jpg").is_file());
    assert!(out_dir.join("imagens/asset_999.tiff").is_file());
    assert!(out_dir.join("imagens/asset_1000.png").is_file());

    let xml_bytes = snapshot_bytes(&out_dir.join("dados.xml"));
    assert_utf8_no_bom_crlf(&xml_bytes);
    assert_illustrator_xml_valid(&String::from_utf8(xml_bytes).unwrap(), 1000);

    assert_file_unchanged(&photo_paths[0], &sample_photo_before);
    assert_file_unchanged(&photo_paths[999], &sample_photo_last_before);
}

#[test]
fn t4_12_xml_special_chars_and_path_encoding_stress() {
    let env = TestEnv::new();
    let p = env
        .services
        .project_service
        .create_project("Stress Escape & Path", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_cod = env
        .services
        .project_service
        .create_variable(&p.id, "Codigo", Some("Codigo"), VariableKind::Text, true)
        .unwrap();
    let v_txt = env
        .services
        .project_service
        .create_variable(&p.id, "Texto", Some("Texto"), VariableKind::Text, true)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&p.id, "Foto", Some("Foto"), VariableKind::LinkedFile, true)
        .unwrap();

    let ds_name = "01 & <Teste> \"Aspas\" 'Simples'";
    let payload = "Ação & Reação <10> \"ok\" 'sim'\nLinha 2 — 00042";
    let recs = seed_records_and_values(
        &env.services,
        &p.id,
        &[(ds_name.into(), vec![(&v_cod, "00042"), (&v_txt, payload)])],
    );
    env.services
        .photo_service
        .link_manual(&p.id, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    let out_dir = env.path().join("Saída (Crachás & Crachás) #2");
    env.services.export_service.export_package(&p.id, &out_dir, false).unwrap();
    let xml1 = std::fs::read_to_string(out_dir.join("dados.xml")).unwrap();
    assert_illustrator_xml_valid(&xml1, 1);
    let ds1 = extract_xml_datasets(&xml1);
    assert_eq!(ds1[0].0, ds_name);
    assert_eq!(ds1[0].1.get("Codigo").map(String::as_str), Some("00042"));
    assert_eq!(ds1[0].1.get("Texto").map(String::as_str), Some(payload));

    let moved_dir = env.path().join("Saída Movida (Crachás & Teste) #3");
    std::fs::rename(&out_dir, &moved_dir).unwrap();
    env.services
        .export_service
        .update_exported_prefix(&moved_dir)
        .unwrap();
    let xml2 = std::fs::read_to_string(moved_dir.join("dados.xml")).unwrap();
    assert_illustrator_xml_valid(&xml2, 1);
    let ds2 = extract_xml_datasets(&xml2);
    assert_eq!(ds2[0].1.get("Texto").map(String::as_str), Some(payload));
    let moved_photo = ds2[0].1.get("Foto").unwrap();
    assert!(!moved_photo.starts_with("file://"));
    assert!(moved_photo.ends_with(r"Saída Movida (Crachás & Teste) #3\imagens\asset_001.jpg"));
}
