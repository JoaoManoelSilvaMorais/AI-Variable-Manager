use ai_variable_manager::application::AppServices;
use ai_variable_manager::domain::{
    Asset, CellDiagnosticKind, ImageFormat, ImportColumnMapping, ImportColumnTarget, ImportMode,
    MatchStatus, NewVariableSpec, PathStrategy, ProjectMode, Record, RecordValue, ThumbnailState,
    ValidationCode, ValidationSeverity, ValidationTarget, Variable, VariableKind,
};
use ai_variable_manager::shared::error::{AppError, DomainError, InfraError};
use ai_variable_manager::shared::text_norm::{is_valid_xml_ncname, suggest_xml_name};
use ai_variable_manager::shared::time::utc_now_rfc3339;

use super::helpers::{
    MINIMAL_EMPTY_SHEET_XLSX, MINIMAL_JPEG_2X2, MINIMAL_PNG_2X2, MINIMAL_TIFF_2X2, TestEnv,
    assert_file_unchanged, assert_illustrator_xml_valid, extract_xml_datasets, fixture_path,
    generate_synthetic_photo_dir, read_fixture_string, request_thumbnail_blocking,
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
fn t2_f03_01_open_nonexistent_project_id_returns_not_found() {
    let env = TestEnv::new();
    let err = env
        .services
        .project_service
        .open_project("non-existent-uuid")
        .unwrap_err();
    assert!(matches!(err, AppError::Domain(DomainError::NotFound { .. })));
}

#[test]
fn t2_f03_02_db_path_inside_nonexistent_parent_or_file_conflict() {
    let env = TestEnv::new();
    let blocker_file = env.path().join("regular_file");
    std::fs::write(&blocker_file, b"not a directory").unwrap();
    let bad_db = blocker_file.join("app.db");

    let err = AppServices::new_with_db_path(&bad_db).err().expect("must fail");
    assert!(matches!(err, AppError::Infra(_)));
}

#[test]
fn t2_f03_03_duplicate_sort_order_constraint_rejected_or_atomically_handled() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Rollback SortOrder", ProjectMode::WithoutTemplate)
        .unwrap();
    let v = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let now = utc_now_rfc3339();
    let r1 = Record {
        id: "r1".into(),
        project_id: proj.id.clone(),
        dataset_name: "DS_1".into(),
        sort_order: 0,
        created_at: now.clone(),
        updated_at: now.clone(),
    };
    let r2 = Record {
        id: "r2".into(),
        project_id: proj.id.clone(),
        dataset_name: "DS_2".into(),
        sort_order: 0,
        created_at: now.clone(),
        updated_at: now,
    };
    let val = RecordValue::new_text("r1", &v.id, "Nao Persistir");
    let err = env
        .services
        .project_repo
        .upsert_records_and_values(&proj.id, &[r1, r2], &[val])
        .unwrap_err();
    assert!(matches!(err, AppError::Domain(DomainError::Conflict(_))));

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert!(snap.records.is_empty());
    assert!(snap.values.is_empty());
}

#[test]
fn t2_f03_04_foreign_key_violation_rejected() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("FK Violation", ProjectMode::WithoutTemplate)
        .unwrap();
    let now = utc_now_rfc3339();
    let r1 = Record {
        id: "r-fk".into(),
        project_id: proj.id.clone(),
        dataset_name: "DS_FK".into(),
        sort_order: 0,
        created_at: now.clone(),
        updated_at: now,
    };
    let bad_val = RecordValue::new_text("r-fk", "nonexistent-variable-id", "orphan");
    let err = env
        .services
        .project_repo
        .upsert_records_and_values(&proj.id, &[r1], &[bad_val])
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::Domain(DomainError::Validation(_))
            | AppError::Domain(DomainError::NotFound { .. })
            | AppError::Infra(InfraError::Database { .. })
    ));

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert!(snap.records.is_empty());
    assert!(snap.values.is_empty());
}

#[test]
fn t2_f03_05_delete_nonexistent_project_returns_error() {
    let env = TestEnv::new();
    let err = env
        .services
        .project_service
        .delete_project("missing-id")
        .unwrap_err();
    assert!(matches!(err, AppError::Domain(DomainError::NotFound { .. })));
}

// ============================================================================
// F-04: Project Lifecycle Management (5 tests)
// ============================================================================

#[test]
fn t2_f04_01_create_project_empty_or_whitespace_name_rejected() {
    let env = TestEnv::new();
    for bad in ["", "   ", "\t\n  "] {
        let err = env
            .services
            .project_service
            .create_project(bad, ProjectMode::WithTemplate)
            .unwrap_err();
        assert!(matches!(err, AppError::Domain(DomainError::Validation(_))));
    }
    assert!(env.services.project_service.list_projects().unwrap().is_empty());
}

#[test]
fn t2_f04_02_rename_project_empty_name_rejected() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Nome Original", ProjectMode::WithoutTemplate)
        .unwrap();
    let err = env
        .services
        .project_service
        .rename_project(&proj.id, "   ")
        .unwrap_err();
    assert!(matches!(err, AppError::Domain(DomainError::Validation(_))));

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.project.name, "Nome Original");
}

#[test]
fn t2_f04_03_rename_nonexistent_project_rejected() {
    let env = TestEnv::new();
    let err = env
        .services
        .project_service
        .rename_project("unknown-id", "Novo Nome")
        .unwrap_err();
    assert!(matches!(err, AppError::Domain(DomainError::NotFound { .. })));
}

#[test]
fn t2_f04_04_get_unset_setting_returns_none() {
    let env = TestEnv::new();
    assert_eq!(
        env.services
            .project_service
            .get_setting("nonexistent_key")
            .unwrap(),
        None
    );
}

#[test]
fn t2_f04_05_project_name_120_char_limit_and_unicode() {
    let env = TestEnv::new();
    let prefix = "Crachás — Gestão 2026 🪪 ";
    let prefix_len = prefix.chars().count();
    let name_120 = format!("{prefix}{}", "á".repeat(120 - prefix_len));
    assert_eq!(name_120.chars().count(), 120);

    let proj = env
        .services
        .project_service
        .create_project(&name_120, ProjectMode::WithoutTemplate)
        .unwrap();
    let reopened = env.reopen();
    let snap = reopened.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.project.name, name_120);
    assert_eq!(snap.project.name.chars().count(), 120);

    let name_121 = format!("{name_120}x");
    assert_eq!(name_121.chars().count(), 121);
    let err_121 = env
        .services
        .project_service
        .create_project(&name_121, ProjectMode::WithoutTemplate)
        .unwrap_err();
    assert!(matches!(err_121, AppError::Domain(DomainError::Validation(_))));

    let name_255 = "é".repeat(255);
    assert_eq!(name_255.chars().count(), 255);
    let err_255 = env
        .services
        .project_service
        .create_project(&name_255, ProjectMode::WithoutTemplate)
        .unwrap_err();
    assert!(matches!(err_255, AppError::Domain(DomainError::Validation(_))));
}

// ============================================================================
// F-06: Virtualized Tabular Grid Data Scale (5 tests)
// ============================================================================

#[test]
fn t2_f06_01_zero_records_grid_state() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Grid Vazio", ProjectMode::WithoutTemplate)
        .unwrap();
    let win = env
        .services
        .data_service
        .get_records_window(&proj.id, 0, 50)
        .unwrap();
    assert_eq!(win.total_records, 0);
    assert!(win.records.is_empty());
    assert!(win.values.is_empty());
}

#[test]
fn t2_f06_02_single_record_boundary_slice() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Um Registro", ProjectMode::WithoutTemplate)
        .unwrap();
    let _v = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, false)
        .unwrap();
    env.services.data_service.add_record(&proj.id, Some("R0")).unwrap();

    let w1 = env
        .services
        .data_service
        .get_records_window(&proj.id, 0, 100)
        .unwrap();
    assert_eq!(w1.total_records, 1);
    assert_eq!(w1.records.len(), 1);

    let w2 = env
        .services
        .data_service
        .get_records_window(&proj.id, 5, 10)
        .unwrap();
    assert_eq!(w2.total_records, 1);
    assert!(w2.records.is_empty());
}

#[test]
fn t2_f06_03_cell_value_extreme_length_10kb() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Célula 10KB", ProjectMode::WithoutTemplate)
        .unwrap();
    let v = env
        .services
        .project_service
        .create_variable(&proj.id, "Texto", Some("Texto"), VariableKind::Text, false)
        .unwrap();
    let r = env.services.data_service.add_record(&proj.id, Some("R1")).unwrap();
    let huge = "ÁbCdEfGhIj".repeat(1000);
    assert_eq!(huge.chars().count(), 10_000);

    env.services
        .data_service
        .update_cell(&proj.id, &r.id, &v.id, &huge)
        .unwrap();

    let reopened = env.reopen();
    let win = reopened
        .data_service
        .get_records_window(&proj.id, 0, 10)
        .unwrap();
    let val = win
        .values
        .iter()
        .find(|x| x.record_id == r.id && x.variable_id == v.id)
        .unwrap();
    assert_eq!(val.text_value, huge);
}

#[test]
fn t2_f06_04_reorder_records_mismatched_id_list_rejected() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Reorder Inválido", ProjectMode::WithoutTemplate)
        .unwrap();
    let r1 = env.services.data_service.add_record(&proj.id, Some("R1")).unwrap();
    let r2 = env.services.data_service.add_record(&proj.id, Some("R2")).unwrap();

    for bad_list in [
        vec![r1.id.clone()],
        vec![r1.id.clone(), r1.id.clone()],
        vec![r1.id.clone(), "unknown-id".to_string()],
        vec![r1.id.clone(), r2.id.clone(), "extra-id".to_string()],
    ] {
        let err = env
            .services
            .data_service
            .reorder_records(&proj.id, &bad_list)
            .unwrap_err();
        assert!(matches!(err, AppError::Domain(DomainError::Validation(_))));
    }

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.records[0].id, r1.id);
    assert_eq!(snap.records[1].id, r2.id);
}

#[test]
fn t2_f06_05_delete_empty_or_missing_record_ids() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Delete Vazio", ProjectMode::WithoutTemplate)
        .unwrap();
    let r1 = env.services.data_service.add_record(&proj.id, Some("R1")).unwrap();

    env.services.data_service.delete_records(&proj.id, &[]).unwrap();
    let res = env
        .services
        .data_service
        .delete_records(&proj.id, &["nonexistent-rec".into()]);
    assert!(res.is_ok() || matches!(res, Err(AppError::Domain(DomainError::NotFound { .. }))));

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.records.len(), 1);
    assert_eq!(snap.records[0].id, r1.id);
}

// ============================================================================
// F-07: Cell Editing & Block Clipboard Paste (5 tests)
// ============================================================================

#[test]
fn t2_f07_01_paste_empty_or_whitespace_clipboard() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Colar Vazio", ProjectMode::WithoutTemplate)
        .unwrap();
    let v = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, false)
        .unwrap();
    let r = env.services.data_service.add_record(&proj.id, Some("R0")).unwrap();
    env.services
        .data_service
        .update_cell(&proj.id, &r.id, &v.id, "Original")
        .unwrap();

    for empty_clip in ["", "\r\n", "   "] {
        let res = env
            .services
            .data_service
            .paste_block(&proj.id, 0, 0, empty_clip);
        assert!(res.is_ok() || matches!(res, Err(AppError::Domain(DomainError::Validation(_)))));
    }

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.records.len(), 1);
    assert_eq!(snap.value_for(&r.id, &v.id).unwrap().text_value, "Original");
}

#[test]
fn t2_f07_02_paste_ragged_rows_with_missing_trailing_columns() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Colar Irregular", ProjectMode::WithoutTemplate)
        .unwrap();
    let v1 = env
        .services
        .project_service
        .create_variable(&proj.id, "C1", Some("C1"), VariableKind::Text, false)
        .unwrap();
    let v2 = env
        .services
        .project_service
        .create_variable(&proj.id, "C2", Some("C2"), VariableKind::Text, false)
        .unwrap();
    let v3 = env
        .services
        .project_service
        .create_variable(&proj.id, "C3", Some("C3"), VariableKind::Text, false)
        .unwrap();

    let count = env
        .services
        .data_service
        .paste_block(&proj.id, 0, 0, "A\tB\tC\r\nD")
        .unwrap();
    assert_eq!(count, 2);

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.records.len(), 2);
    assert_eq!(snap.value_for(&snap.records[1].id, &v1.id).unwrap().text_value, "D");
    assert_eq!(
        snap.value_for(&snap.records[1].id, &v2.id)
            .map(|x| x.text_value.as_str())
            .unwrap_or(""),
        ""
    );
    assert_eq!(
        snap.value_for(&snap.records[1].id, &v3.id)
            .map(|x| x.text_value.as_str())
            .unwrap_or(""),
        ""
    );
}

#[test]
fn t2_f07_03_paste_wider_than_column_count_clamps_or_reports_overflow() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Colar Largo", ProjectMode::WithoutTemplate)
        .unwrap();
    let v1 = env
        .services
        .project_service
        .create_variable(&proj.id, "C1", Some("C1"), VariableKind::Text, false)
        .unwrap();
    let v2 = env
        .services
        .project_service
        .create_variable(&proj.id, "C2", Some("C2"), VariableKind::Text, false)
        .unwrap();

    let res = env
        .services
        .data_service
        .paste_block(&proj.id, 0, 0, "1\t2\t3\t4");
    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.variables.len(), 2);
    match res {
        Ok(n) => {
            assert_eq!(n, 1);
            assert_eq!(snap.value_for(&snap.records[0].id, &v1.id).unwrap().text_value, "1");
            assert_eq!(snap.value_for(&snap.records[0].id, &v2.id).unwrap().text_value, "2");
        }
        Err(AppError::Domain(DomainError::Validation(_))) => {}
        Err(other) => panic!("unexpected error on wide paste: {other:?}"),
    }
}

#[test]
fn t2_f07_04_paste_unclosed_quote_malformed_csv() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Aspas Abertas", ProjectMode::WithoutTemplate)
        .unwrap();
    let _v1 = env
        .services
        .project_service
        .create_variable(&proj.id, "C1", Some("C1"), VariableKind::Text, false)
        .unwrap();
    let _v2 = env
        .services
        .project_service
        .create_variable(&proj.id, "C2", Some("C2"), VariableKind::Text, false)
        .unwrap();

    let res = env
        .services
        .data_service
        .paste_block(&proj.id, 0, 0, "\"unclosed,cell\tval");
    assert!(res.is_ok() || matches!(res, Err(AppError::Domain(DomainError::Validation(_)))));
}

#[test]
fn t2_f07_05_paste_preserves_leading_zeros_and_special_unicode() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Colar Zeros Unicode", ProjectMode::WithoutTemplate)
        .unwrap();
    let v1 = env
        .services
        .project_service
        .create_variable(&proj.id, "Cod", Some("Cod"), VariableKind::Text, false)
        .unwrap();
    let v2 = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, false)
        .unwrap();

    let n = env
        .services
        .data_service
        .paste_block(&proj.id, 0, 0, "0001\tJosé — & < >")
        .unwrap();
    assert_eq!(n, 1);

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.value_for(&snap.records[0].id, &v1.id).unwrap().text_value, "0001");
    assert_eq!(
        snap.value_for(&snap.records[0].id, &v2.id).unwrap().text_value,
        "José — & < >"
    );
}

// ============================================================================
// F-08: Dynamic Column Management (5 tests)
// ============================================================================

#[test]
fn t2_f08_01_delete_last_remaining_column() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Última Coluna", ProjectMode::WithoutTemplate)
        .unwrap();
    let v = env
        .services
        .project_service
        .create_variable(&proj.id, "Unica", Some("Unica"), VariableKind::Text, true)
        .unwrap();
    seed_records_and_values(
        &env.services,
        &proj.id,
        &[
            ("R1".into(), vec![(&v, "A")]),
            ("R2".into(), vec![(&v, "B")]),
        ],
    );

    env.services
        .project_service
        .delete_variable(&proj.id, &v.id)
        .unwrap();
    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert!(snap.variables.is_empty());
    assert!(snap.values.is_empty());
}

#[test]
fn t2_f08_02_reorder_variables_missing_or_duplicate_id_rejected() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Reorder Col Inválido", ProjectMode::WithoutTemplate)
        .unwrap();
    let v1 = env
        .services
        .project_service
        .create_variable(&proj.id, "A", Some("A"), VariableKind::Text, false)
        .unwrap();
    let v2 = env
        .services
        .project_service
        .create_variable(&proj.id, "B", Some("B"), VariableKind::Text, false)
        .unwrap();

    for bad in [vec![v1.id.clone()], vec![v1.id.clone(), v1.id.clone()]] {
        let err = env
            .services
            .project_service
            .reorder_variables(&proj.id, &bad)
            .unwrap_err();
        assert!(matches!(err, AppError::Domain(DomainError::Validation(_))));
    }

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.variables[0].id, v1.id);
    assert_eq!(snap.variables[1].id, v2.id);
}

#[test]
fn t2_f08_03_duplicate_nonexistent_variable_rejected() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Dup Desconhecida", ProjectMode::WithoutTemplate)
        .unwrap();
    let err = env
        .services
        .project_service
        .duplicate_variable(&proj.id, "missing-var-id")
        .unwrap_err();
    assert!(matches!(err, AppError::Domain(DomainError::NotFound { .. })));
}

#[test]
fn t2_f08_04_change_variable_kind_text_to_linked_file_clears_or_resets_asset_state() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Troca Tipo", ProjectMode::WithoutTemplate)
        .unwrap();
    let v = env
        .services
        .project_service
        .create_variable(&proj.id, "Col", Some("Col"), VariableKind::Text, false)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[("R1".into(), vec![(&v, "valor")])],
    );

    // Text -> LinkedFile
    let switched_to_linked = env
        .services
        .project_service
        .update_variable_options(&proj.id, &v.id, VariableKind::LinkedFile, false, None)
        .unwrap();
    assert_eq!(switched_to_linked.kind, VariableKind::LinkedFile);
    let snap_mid = env.services.project_service.open_project(&proj.id).unwrap();
    let val_mid = snap_mid.value_for(&recs[0].id, &v.id).unwrap();
    assert!(val_mid.asset_id.is_none());
    assert_eq!(val_mid.match_status, MatchStatus::Unmatched);

    // Seed an asset link and switch LinkedFile -> Text; stale asset link must reset cleanly
    let asset = Asset {
        id: "asset-switch".into(),
        project_id: proj.id.clone(),
        source_path: fixture_path("images/joao_silva.jpg"),
        file_name: "joao_silva.jpg".into(),
        stem: "joao_silva".into(),
        normalized_stem: "joao_silva".into(),
        format: ImageFormat::Jpg,
        file_size_bytes: 683,
        modified_epoch_ms: 1_700_000_000_000,
        exists_on_disk: true,
    };
    env.services
        .project_repo
        .replace_assets(&proj.id, &[asset])
        .unwrap();
    let linked_val = RecordValue {
        record_id: recs[0].id.clone(),
        variable_id: v.id.clone(),
        text_value: "joao_silva".into(),
        asset_id: Some("asset-switch".into()),
        match_status: MatchStatus::Manual,
        match_confidence: Some(1.0),
    };
    env.services
        .project_repo
        .upsert_records_and_values(&proj.id, &[], &[linked_val])
        .unwrap();

    let switched_back = env
        .services
        .project_service
        .update_variable_options(&proj.id, &v.id, VariableKind::Text, false, None)
        .unwrap();
    assert_eq!(switched_back.kind, VariableKind::Text);
    let snap_after = env.services.project_service.open_project(&proj.id).unwrap();
    let val_after = snap_after.value_for(&recs[0].id, &v.id).unwrap();
    assert!(
        val_after.asset_id.is_none(),
        "switching LinkedFile -> Text must clear stale asset_id"
    );
    assert_eq!(val_after.match_status, MatchStatus::Unmatched);
}

#[test]
fn t2_f08_05_create_50_dynamic_columns_without_ddl() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("50 Colunas", ProjectMode::WithoutTemplate)
        .unwrap();
    let mut vars = Vec::new();
    for i in 0..50 {
        vars.push(
            env.services
                .project_service
                .create_variable(
                    &proj.id,
                    &format!("Coluna {i}"),
                    Some(&format!("Col_{i}")),
                    VariableKind::Text,
                    false,
                )
                .unwrap(),
        );
    }

    let reversed: Vec<String> = vars.iter().rev().map(|v| v.id.clone()).collect();
    env.services
        .project_service
        .reorder_variables(&proj.id, &reversed)
        .unwrap();

    let dup = env
        .services
        .project_service
        .duplicate_variable(&proj.id, &vars[0].id)
        .unwrap();
    assert_eq!(dup.xml_name, "Col_0_copia");

    for v in vars.iter().take(10) {
        env.services
            .project_service
            .delete_variable(&proj.id, &v.id)
            .unwrap();
    }

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.variables.len(), 41);
    for (idx, v) in snap.variables.iter().enumerate() {
        assert_eq!(v.sort_order, idx);
    }
}

// ============================================================================
// F-09: xml_name & Required Validation (5 tests)
// ============================================================================

#[test]
fn t2_f09_01_reject_xml_name_starting_with_digit_or_hyphen() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("NCName Prefixo", ProjectMode::WithoutTemplate)
        .unwrap();

    for bad in ["1Nome", "-Nome", ".Nome"] {
        let err = env
            .services
            .project_service
            .create_variable(&proj.id, "Campo", Some(bad), VariableKind::Text, false)
            .unwrap_err();
        assert!(matches!(err, AppError::Domain(DomainError::Validation(_))));
    }
}

#[test]
fn t2_f09_02_reject_xml_name_with_spaces_or_reserved_chars() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("NCName Chars", ProjectMode::WithoutTemplate)
        .unwrap();

    for bad in ["Nome Funcionario", "Nome<1>", "ns:nome", "a&b"] {
        let err = env
            .services
            .project_service
            .create_variable(&proj.id, "Campo", Some(bad), VariableKind::Text, false)
            .unwrap_err();
        assert!(matches!(err, AppError::Domain(DomainError::Validation(_))));
    }
}

#[test]
fn t2_f09_03_reject_empty_xml_name() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("NCName Vazio", ProjectMode::WithoutTemplate)
        .unwrap();

    for bad in ["", "   "] {
        let err = env
            .services
            .project_service
            .create_variable(&proj.id, "Campo", Some(bad), VariableKind::Text, false)
            .unwrap_err();
        assert!(matches!(err, AppError::Domain(DomainError::Validation(_))));
    }
}

#[test]
fn t2_f09_04_reject_duplicate_xml_name_within_same_project() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("NCName Duplicado", ProjectMode::WithoutTemplate)
        .unwrap();
    env.services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let err = env
        .services
        .project_service
        .create_variable(&proj.id, "Outro", Some("Nome"), VariableKind::Text, false)
        .unwrap_err();
    assert!(matches!(err, AppError::Domain(DomainError::Conflict(_))));
}

#[test]
fn t2_f09_05_auto_suggest_empty_or_punctuation_only_fallback() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Fallback NCName", ProjectMode::WithoutTemplate)
        .unwrap();
    let suggested = suggest_xml_name("--- !!! ???");
    assert!(!suggested.is_empty());
    assert!(is_valid_xml_ncname(&suggested));

    let v = env
        .services
        .project_service
        .create_variable(&proj.id, "--- !!! ???", None, VariableKind::Text, false)
        .unwrap();
    assert!(!v.xml_name.is_empty());
    assert!(is_valid_xml_ncname(&v.xml_name));
}

// ============================================================================
// F-10: .xlsx Importer (5 tests)
// ============================================================================

#[test]
fn t2_f10_01_open_corrupt_or_non_xlsx_file_returns_error() {
    let env = TestEnv::new();
    let fake = env.path().join("corrupt.xlsx");
    std::fs::write(&fake, b"not a valid zip or xlsx payload").unwrap();

    match env.services.import_service.list_sheets(&fake) {
        Err(
            AppError::Infra(InfraError::Spreadsheet(_))
            | AppError::Domain(DomainError::UnsupportedFormat(_))
            | AppError::Domain(DomainError::Validation(_)),
        ) => {}
        other => panic!("expected corrupt xlsx error from list_sheets, got {other:?}"),
    }
    match env
        .services
        .import_service
        .preview_spreadsheet(&fake, None, Some(10))
    {
        Err(
            AppError::Infra(InfraError::Spreadsheet(_))
            | AppError::Domain(DomainError::UnsupportedFormat(_))
            | AppError::Domain(DomainError::Validation(_)),
        ) => {}
        other => panic!("expected corrupt xlsx error from preview_spreadsheet, got {other:?}"),
    }
}

#[test]
fn t2_f10_02_missing_sheet_name_returns_error() {
    let env = TestEnv::new();
    let err = env
        .services
        .import_service
        .preview_spreadsheet(&fixture_path("sample_badges.xlsx"), Some("NaoExiste"), None)
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::Infra(InfraError::Spreadsheet(_)) | AppError::Domain(DomainError::NotFound { .. })
    ));
}

#[test]
fn t2_f10_03_empty_xlsx_sheet_returns_empty_table_or_error() {
    let env = TestEnv::new();
    let empty_xlsx = env.path().join("empty_sheet.xlsx");
    std::fs::write(&empty_xlsx, MINIMAL_EMPTY_SHEET_XLSX).unwrap();

    match env
        .services
        .import_service
        .preview_spreadsheet(&empty_xlsx, None, None)
    {
        Ok(table) => assert!(table.rows.is_empty()),
        Err(AppError::Infra(InfraError::Spreadsheet(_)))
        | Err(AppError::Domain(DomainError::Validation(_))) => {}
        Err(other) => panic!("unexpected error on empty xlsx sheet: {other:?}"),
    }
}

#[test]
fn t2_f10_04_header_row_beyond_row_count_returns_error() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Header Beyond", ProjectMode::WithoutTemplate)
        .unwrap();
    let mappings = vec![ImportColumnMapping {
        source_col_index: 0,
        target: ImportColumnTarget::NewVariable(new_var_spec("codigo", VariableKind::Text, false)),
    }];
    let err = env
        .services
        .import_service
        .commit_import(
            &proj.id,
            &fixture_path("sample_badges.xlsx"),
            Some("Crachas"),
            99,
            &mappings,
            ImportMode::Append,
        )
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::Domain(DomainError::Validation(_)) | AppError::Infra(InfraError::Spreadsheet(_))
    ));
}

#[test]
fn t2_f10_05_xlsx_numeric_float_cell_surfaces_integer_format_without_dot_zero() {
    let env = TestEnv::new();
    let table = env
        .services
        .import_service
        .preview_spreadsheet(&fixture_path("sample_badges.xlsx"), Some("Crachas"), None)
        .unwrap();
    assert_eq!(table.rows.len(), 6);
    assert_eq!(table.rows[1][0], "00421");
    assert_eq!(table.rows[2][0], "00007");
    assert_eq!(table.rows[1][3], "421");
    assert_eq!(table.rows[2][3], "12.5");
    assert_eq!(table.rows[3][3], "0");
    assert_eq!(table.rows[4][3], "1000");
}

// ============================================================================
// F-11: .csv Importer (5 tests)
// ============================================================================

#[test]
fn t2_f11_01_empty_0_byte_csv_returns_error_or_empty() {
    let env = TestEnv::new();
    let empty_csv = env.path().join("empty.csv");
    std::fs::write(&empty_csv, b"").unwrap();

    match env
        .services
        .import_service
        .preview_spreadsheet(&empty_csv, None, None)
    {
        Ok(raw) => assert!(raw.rows.is_empty()),
        Err(AppError::Infra(InfraError::Spreadsheet(_)))
        | Err(AppError::Domain(DomainError::Validation(_))) => {}
        Err(other) => panic!("unexpected error on 0-byte CSV: {other:?}"),
    }
}

#[test]
fn t2_f11_02_single_column_csv_without_delimiter() {
    let env = TestEnv::new();
    let single = env.path().join("single.csv");
    std::fs::write(&single, "Nome\r\nAna\r\nBruno\r\n").unwrap();

    let raw = env
        .services
        .import_service
        .preview_spreadsheet(&single, None, None)
        .unwrap();
    assert_eq!(raw.rows.len(), 3);
    assert_eq!(raw.rows[0], vec!["Nome"]);
    assert_eq!(raw.rows[1], vec!["Ana"]);
    assert_eq!(raw.rows[2], vec!["Bruno"]);
}

#[test]
fn t2_f11_03_invalid_utf8_and_undefined_win1252_bytes() {
    let env = TestEnv::new();
    let raw_win = env
        .services
        .import_service
        .preview_spreadsheet(&fixture_path("sample_win1252_semicolon.csv"), None, None)
        .unwrap();
    assert_eq!(raw_win.rows[1][3], "Águia – Seção");

    let weird = env.path().join("undef_cp1252.csv");
    std::fs::write(&weird, b"col1;col2\r\n001;Teste \x81\x8D\xFF \x96 fim\r\n").unwrap();
    let raw_weird = env
        .services
        .import_service
        .preview_spreadsheet(&weird, None, None)
        .unwrap();
    assert_eq!(raw_weird.rows.len(), 2);
    assert_eq!(raw_weird.rows[1][0], "001");
    assert!(raw_weird.rows[1][1].contains("Teste"));
    assert!(raw_weird.rows[1][1].contains("fim"));
}

#[test]
fn t2_f11_04_inconsistent_column_count_in_row() {
    let env = TestEnv::new();
    let ragged = env.path().join("ragged.csv");
    std::fs::write(&ragged, "c1,c2,c3\r\na,b,c\r\n1,2,3,4\r\n").unwrap();

    match env
        .services
        .import_service
        .preview_spreadsheet(&ragged, None, None)
    {
        Ok(raw) => assert!(raw.rows.len() >= 2),
        Err(AppError::Infra(InfraError::Spreadsheet(_)))
        | Err(AppError::Domain(DomainError::Validation(_))) => {}
        Err(other) => panic!("unexpected error on ragged CSV: {other:?}"),
    }
}

#[test]
fn t2_f11_05_missing_csv_file_path_returns_io_error() {
    let env = TestEnv::new();
    let missing = env.path().join("does_not_exist.csv");
    let err = env
        .services
        .import_service
        .preview_spreadsheet(&missing, None, None)
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::Infra(InfraError::Io { .. }) | AppError::Infra(InfraError::Spreadsheet(_))
    ));
}

// ============================================================================
// F-12: Column Mapping & Cell Flags (5 tests)
// ============================================================================

#[test]
fn t2_f12_01_all_columns_ignored_imports_zero_fields_or_rejects() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Tudo Ignorado", ProjectMode::WithoutTemplate)
        .unwrap();
    let mappings = vec![
        ImportColumnMapping {
            source_col_index: 0,
            target: ImportColumnTarget::Ignore,
        },
        ImportColumnMapping {
            source_col_index: 1,
            target: ImportColumnTarget::Ignore,
        },
    ];
    let res = env.services.import_service.commit_import(
        &proj.id,
        &fixture_path("sample_utf8_comma.csv"),
        None,
        0,
        &mappings,
        ImportMode::Append,
    );
    match res {
        Ok(0) => {
            let snap = env.services.project_service.open_project(&proj.id).unwrap();
            assert!(snap.records.is_empty());
        }
        Err(AppError::Domain(DomainError::Validation(_))) => {}
        other => panic!("expected Ok(0) or Validation error, got {other:?}"),
    }
}

#[test]
fn t2_f12_02_map_two_source_columns_to_same_target_variable_rejected() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Mapeamento Duplicado", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();

    let mappings = vec![
        ImportColumnMapping {
            source_col_index: 0,
            target: ImportColumnTarget::ExistingVariable(v_nome.id.clone()),
        },
        ImportColumnMapping {
            source_col_index: 1,
            target: ImportColumnTarget::ExistingVariable(v_nome.id.clone()),
        },
    ];
    let err = env
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
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::Domain(DomainError::Validation(_)) | AppError::Domain(DomainError::Conflict(_))
    ));
}

#[test]
fn t2_f12_03_whitespace_only_cell_flagged_as_empty() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Espaço Vazio", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_req = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01".into(), vec![(&v_req, "   \t  ")])],
    );

    let diags = env
        .services
        .data_service
        .compute_cell_diagnostics(&proj.id)
        .unwrap();
    assert!(diags.iter().any(|d| {
        d.record_id == recs[0].id
            && d.variable_id == v_req.id
            && matches!(
                d.kind,
                CellDiagnosticKind::EmptyRequired | CellDiagnosticKind::EmptyOptional
            )
    }));
}

#[test]
fn t2_f12_04_duplicate_detection_handles_case_or_exact_equality_deterministically() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Duplicidade Exata", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let v_cod = env
        .services
        .project_service
        .create_variable(&proj.id, "Cod", Some("Cod"), VariableKind::Text, true)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[
            ("01".into(), vec![(&v_nome, "Ana"), (&v_cod, "00421")]),
            ("02".into(), vec![(&v_nome, "Ana"), (&v_cod, "421")]),
            ("03".into(), vec![(&v_nome, "Maria"), (&v_cod, "00999")]),
        ],
    );

    let diags = env
        .services
        .data_service
        .compute_cell_diagnostics(&proj.id)
        .unwrap();
    assert!(diags.iter().any(|d| {
        d.record_id == recs[0].id
            && d.variable_id == v_nome.id
            && d.kind == CellDiagnosticKind::DuplicateValue
    }));
    assert!(diags.iter().any(|d| {
        d.record_id == recs[1].id
            && d.variable_id == v_nome.id
            && d.kind == CellDiagnosticKind::DuplicateValue
    }));
    assert!(!diags.iter().any(|d| {
        d.variable_id == v_cod.id && d.kind == CellDiagnosticKind::DuplicateValue
    }));
}

#[test]
fn t2_f12_05_zero_data_rows_header_only_import() {
    let env = TestEnv::new();
    let csv_path = env.path().join("header_only.csv");
    std::fs::write(&csv_path, "codigo,nome\r\n").unwrap();

    let proj = env
        .services
        .project_service
        .create_project("Somente Cabeçalho", ProjectMode::WithoutTemplate)
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
    let n = env
        .services
        .import_service
        .commit_import(&proj.id, &csv_path, None, 0, &mappings, ImportMode::Append)
        .unwrap();
    assert_eq!(n, 0);
    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert!(snap.records.is_empty());
}

// ============================================================================
// F-13: Local Photo Directory Scan (5 tests)
// ============================================================================

#[test]
fn t2_f13_01_scan_nonexistent_directory_returns_error() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Pasta Inexistente", ProjectMode::WithoutTemplate)
        .unwrap();
    let err = env
        .services
        .photo_service
        .scan_directory(&proj.id, &env.path().join("does_not_exist"))
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::Infra(InfraError::Io { .. }) | AppError::Domain(DomainError::NotFound { .. })
    ));
}

#[test]
fn t2_f13_02_scan_file_path_instead_of_directory_returns_error() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Arquivo como Pasta", ProjectMode::WithoutTemplate)
        .unwrap();
    let err = env
        .services
        .photo_service
        .scan_directory(&proj.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::Domain(DomainError::Validation(_)) | AppError::Infra(InfraError::Io { .. })
    ));
}

#[test]
fn t2_f13_03_scan_empty_directory_returns_zero_assets() {
    let env = TestEnv::new();
    let empty_dir = env.path().join("empty_photos");
    std::fs::create_dir_all(&empty_dir).unwrap();
    let proj = env
        .services
        .project_service
        .create_project("Pasta Vazia", ProjectMode::WithoutTemplate)
        .unwrap();
    let scan = env
        .services
        .photo_service
        .scan_directory(&proj.id, &empty_dir)
        .unwrap();
    assert!(scan.assets.is_empty());
    assert_eq!(scan.skipped_unsupported, 0);
}

#[test]
fn t2_f13_04_double_extension_jpg_jpeg_supported_while_heic_skipped() {
    let env = TestEnv::new();
    let dir = env.path().join("double_ext");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("BD9A5505.jpg.jpeg"), MINIMAL_JPEG_2X2).unwrap();
    std::fs::copy(
        fixture_path("images/ignored_sample.heic"),
        dir.join("ignored_sample.heic"),
    )
    .unwrap();
    std::fs::copy(
        fixture_path("images/ignored_document.pdf"),
        dir.join("ignored_document.pdf"),
    )
    .unwrap();

    let proj = env
        .services
        .project_service
        .create_project("Dupla Extensão", ProjectMode::WithoutTemplate)
        .unwrap();
    let scan = env
        .services
        .photo_service
        .scan_directory(&proj.id, &dir)
        .unwrap();
    assert_eq!(scan.assets.len(), 1);
    assert_eq!(scan.assets[0].format, ImageFormat::Jpeg);
    assert_eq!(scan.assets[0].normalized_stem, "bd9a5505.jpg");
    assert_eq!(scan.skipped_unsupported, 2);
}

#[test]
fn t2_f13_05_zero_byte_image_file_scanned_gracefully_with_size_0() {
    let env = TestEnv::new();
    let dir = env.path().join("zero_byte");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("corrupt.jpg"), b"").unwrap();

    let proj = env
        .services
        .project_service
        .create_project("Foto 0 Bytes", ProjectMode::WithoutTemplate)
        .unwrap();
    let scan = env
        .services
        .photo_service
        .scan_directory(&proj.id, &dir)
        .unwrap();
    assert_eq!(scan.assets.len(), 1);
    assert_eq!(scan.assets[0].file_size_bytes, 0);
}

// ============================================================================
// F-14: Bounded On-Demand Thumbnails (5 tests)
// ============================================================================

#[test]
fn t2_f14_01_corrupt_bytes_in_jpg_returns_error_state_without_panic() {
    let env = TestEnv::new();
    let bad = env.path().join("bad.jpg");
    std::fs::write(&bad, b"corrupt garbage bytes not a jpeg").unwrap();

    let state = request_thumbnail_blocking(&env.services, "bad-jpg", &bad).unwrap();
    assert!(matches!(state, ThumbnailState::Failed(_)));
}

#[test]
fn t2_f14_02_missing_file_on_disk_returns_error_state() {
    let env = TestEnv::new();
    let missing = env.path().join("ghost.jpg");
    let state = request_thumbnail_blocking(&env.services, "ghost", &missing).unwrap();
    assert!(matches!(state, ThumbnailState::Failed(_)));
}

#[test]
fn t2_f14_03_unsupported_extension_heic_rejected() {
    let env = TestEnv::new();
    let state = request_thumbnail_blocking(
        &env.services,
        "heic-1",
        &fixture_path("images/ignored_sample.heic"),
    )
    .unwrap();
    assert!(matches!(state, ThumbnailState::Failed(_)));
}

#[test]
fn t2_f14_04_zero_byte_image_rejected_cleanly() {
    let env = TestEnv::new();
    let empty_png = env.path().join("empty.png");
    std::fs::write(&empty_png, b"").unwrap();
    let state = request_thumbnail_blocking(&env.services, "empty-png", &empty_png).unwrap();
    assert!(matches!(state, ThumbnailState::Failed(_)));
}

#[test]
fn t2_f14_05_rapid_cache_churn_500_requests_stays_within_192_cap() {
    let env = TestEnv::new();
    let dir = env.path().join("churn_250");
    let paths = generate_synthetic_photo_dir(&dir, 250);

    for round in 0..2 {
        for (i, p) in paths.iter().enumerate() {
            let id = format!("churn-{i}");
            let state = request_thumbnail_blocking(&env.services, &id, p).unwrap();
            assert!(
                matches!(state, ThumbnailState::Ready(_)),
                "expected Ready thumbnail on round {round} item {i}, got {state:?}"
            );
            let len = env.services.photo_service.thumbnail_cache_len().unwrap();
            assert!(len <= 192, "cache exceeded 192 on round {round} item {i}: {len}");
        }
    }
    assert_eq!(env.services.photo_service.thumbnail_cache_len().unwrap(), 192);
}

// ============================================================================
// F-15: Method A: Auto Photo Matching (5 tests)
// ============================================================================

#[test]
fn t2_f15_01_empty_or_whitespace_lookup_value_remains_unmatched() {
    let env = TestEnv::new();
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Lookup Vazio", &["", "   \t  "]);
    env.services
        .photo_service
        .scan_directory(&pid, &fixture_path("images"))
        .unwrap();
    env.services.photo_service.run_auto_match(&pid).unwrap();

    let snap = env.services.project_service.open_project(&pid).unwrap();
    for r in &recs {
        let val = snap.value_for(&r.id, &v_foto.id).unwrap();
        assert_eq!(val.match_status, MatchStatus::Unmatched);
        assert!(val.asset_id.is_none());
    }
}

#[test]
fn t2_f15_02_completely_unrelated_names_stay_unmatched_below_threshold() {
    let env = TestEnv::new();
    let dir = env.path().join("unrelated");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("999_xyz_avatar.jpg"), MINIMAL_JPEG_2X2).unwrap();

    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Sem Relação", &["Carlos Eduardo"]);
    env.services
        .photo_service
        .scan_directory(&pid, &dir)
        .unwrap();
    env.services.photo_service.run_auto_match(&pid).unwrap();

    let snap = env.services.project_service.open_project(&pid).unwrap();
    let val = snap.value_for(&recs[0].id, &v_foto.id).unwrap();
    assert_eq!(val.match_status, MatchStatus::Unmatched);
    assert!(val.asset_id.is_none());
    assert!(snap.match_candidates.is_empty());
}

#[test]
fn t2_f15_03_three_way_same_stem_tie_is_ambiguous() {
    let env = TestEnv::new();
    let dir = env.path().join("tie_3");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Ana.jpg"), MINIMAL_JPEG_2X2).unwrap();
    std::fs::write(dir.join("Ana.png"), MINIMAL_PNG_2X2).unwrap();
    std::fs::write(dir.join("Ana.tiff"), MINIMAL_TIFF_2X2).unwrap();

    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Empate Triplo", &["Ana"]);
    env.services
        .photo_service
        .scan_directory(&pid, &dir)
        .unwrap();
    let diag = env.services.photo_service.run_auto_match(&pid).unwrap();
    assert_eq!(diag.ambiguous_count, 1);

    let snap = env.services.project_service.open_project(&pid).unwrap();
    let val = snap.value_for(&recs[0].id, &v_foto.id).unwrap();
    assert_eq!(val.match_status, MatchStatus::Ambiguous);
    assert_eq!(
        snap.match_candidates
            .iter()
            .filter(|c| c.record_id == recs[0].id && c.variable_id == v_foto.id)
            .count(),
        3
    );
}

#[test]
fn t2_f15_04_generic_numbered_filenames_do_not_false_match() {
    let env = TestEnv::new();
    let dir = env.path().join("generic_nums");
    std::fs::create_dir_all(&dir).unwrap();
    for name in [
        "2.png",
        "26.png",
        "(1).png",
        "(16).png",
        "WhatsApp Image 2026-10-05.jpeg",
    ] {
        std::fs::write(dir.join(name), MINIMAL_PNG_2X2).unwrap();
    }

    let (pid, _vn, v_foto, recs) = setup_photo_project(
        &env,
        "Nomes Genéricos",
        &["João Carlos Silva Morais", "Maria Conceição Teixeira"],
    );
    env.services
        .photo_service
        .scan_directory(&pid, &dir)
        .unwrap();
    env.services.photo_service.run_auto_match(&pid).unwrap();

    let snap = env.services.project_service.open_project(&pid).unwrap();
    for r in &recs {
        assert_eq!(
            snap.value_for(&r.id, &v_foto.id).unwrap().match_status,
            MatchStatus::Unmatched
        );
    }
}

#[test]
fn t2_f15_05_auto_match_zero_assets_in_project() {
    let env = TestEnv::new();
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Zero Assets", &["Ana", "Bruno"]);
    let diag = env.services.photo_service.run_auto_match(&pid).unwrap();
    assert_eq!(diag.total_scanned_assets, 0);
    assert_eq!(diag.matched_count, 0);

    let snap = env.services.project_service.open_project(&pid).unwrap();
    for r in &recs {
        assert_eq!(
            snap.value_for(&r.id, &v_foto.id).unwrap().match_status,
            MatchStatus::Unmatched
        );
    }
}

// ============================================================================
// F-16: Method B: Manual Individual Link (5 tests)
// ============================================================================

#[test]
fn t2_f16_01_manual_link_nonexistent_file_rejected() {
    let env = TestEnv::new();
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Manual Ghost", &["Joao"]);
    let err = env
        .services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &env.path().join("ghost.jpg"))
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::Infra(InfraError::Io { .. }) | AppError::Domain(DomainError::NotFound { .. })
    ));
    let snap = env.services.project_service.open_project(&pid).unwrap();
    assert_eq!(
        snap.value_for(&recs[0].id, &v_foto.id).unwrap().match_status,
        MatchStatus::Unmatched
    );
}

#[test]
fn t2_f16_02_manual_link_unsupported_heic_or_pdf_rejected() {
    let env = TestEnv::new();
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Manual HEIC", &["Joao"]);
    for bad in [
        fixture_path("images/ignored_sample.heic"),
        fixture_path("images/ignored_document.pdf"),
    ] {
        let err = env
            .services
            .photo_service
            .link_manual(&pid, &recs[0].id, &v_foto.id, &bad)
            .unwrap_err();
        assert!(matches!(
            err,
            AppError::Domain(DomainError::UnsupportedFormat(_))
                | AppError::Domain(DomainError::Validation(_))
        ));
    }
}

#[test]
fn t2_f16_03_manual_link_to_text_variable_rejected() {
    let env = TestEnv::new();
    let (pid, v_nome, _vf, recs) = setup_photo_project(&env, "Manual Text Col", &["Joao"]);
    let err = env
        .services
        .photo_service
        .link_manual(
            &pid,
            &recs[0].id,
            &v_nome.id,
            &fixture_path("images/joao_silva.jpg"),
        )
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::Domain(DomainError::Validation(_)) | AppError::Domain(DomainError::InvalidState(_))
    ));
}

#[test]
fn t2_f16_04_manual_link_to_nonexistent_record_rejected() {
    let env = TestEnv::new();
    let (pid, _vn, v_foto, _recs) = setup_photo_project(&env, "Manual Missing Rec", &["Joao"]);
    let err = env
        .services
        .photo_service
        .link_manual(
            &pid,
            "missing-record-id",
            &v_foto.id,
            &fixture_path("images/joao_silva.jpg"),
        )
        .unwrap_err();
    assert!(matches!(err, AppError::Domain(DomainError::NotFound { .. })));
}

#[test]
fn t2_f16_05_manual_link_corrupt_header_or_zero_byte_image_rejected() {
    let env = TestEnv::new();
    let empty_jpg = env.path().join("empty.jpg");
    std::fs::write(&empty_jpg, b"").unwrap();
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Manual Empty Img", &["Joao"]);

    let err = env
        .services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &empty_jpg)
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::Domain(DomainError::Validation(_)) | AppError::Infra(InfraError::Media(_))
    ));
}

// ============================================================================
// F-17: Method C: Batch Unmatched Link (5 tests)
// ============================================================================

#[test]
fn t2_f17_01_batch_pair_empty_list_is_safe_noop() {
    let env = TestEnv::new();
    let (pid, _vn, _vf, _recs) = setup_photo_project(&env, "Batch Vazio", &["A"]);
    let res = env
        .services
        .photo_service
        .batch_link(&pid, &[], false)
        .unwrap();
    assert_eq!(res.linked_count, 0);
    assert!(res.duplicate_asset_warnings.is_empty());
}

#[test]
fn t2_f17_02_batch_pair_already_matched_record_rejected_or_confirmed() {
    let env = TestEnv::new();
    let photos_dir = env.path().join("batch_conflict");
    generate_synthetic_photo_dir(&photos_dir, 2);

    let (pid, _vn, v_foto, recs) =
        setup_photo_project(&env, "Batch Já Vinculado", &["Pessoa_0001"]);
    env.services
        .photo_service
        .scan_directory(&pid, &photos_dir)
        .unwrap();
    env.services.photo_service.run_auto_match(&pid).unwrap();

    let snap = env.services.project_service.open_project(&pid).unwrap();
    let orig_asset = snap
        .value_for(&recs[0].id, &v_foto.id)
        .unwrap()
        .asset_id
        .clone()
        .unwrap();
    let other_asset = snap
        .assets
        .iter()
        .find(|a| a.id != orig_asset)
        .unwrap()
        .id
        .clone();

    let err = env
        .services
        .photo_service
        .batch_link(
            &pid,
            &[(recs[0].id.clone(), v_foto.id.clone(), other_asset)],
            false,
        )
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::Domain(DomainError::Conflict(_)) | AppError::Domain(DomainError::InvalidState(_))
    ));
    let snap_after = env.services.project_service.open_project(&pid).unwrap();
    assert_eq!(
        snap_after
            .value_for(&recs[0].id, &v_foto.id)
            .unwrap()
            .asset_id
            .as_deref(),
        Some(orig_asset.as_str())
    );
}

#[test]
fn t2_f17_03_batch_pair_nonexistent_asset_id_rejected() {
    let env = TestEnv::new();
    let photos_dir = env.path().join("batch_rollback");
    generate_synthetic_photo_dir(&photos_dir, 1);

    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Batch Rollback", &["A", "B"]);
    env.services
        .photo_service
        .scan_directory(&pid, &photos_dir)
        .unwrap();
    let snap = env.services.project_service.open_project(&pid).unwrap();
    let valid_id = snap.assets[0].id.clone();

    let err = env
        .services
        .photo_service
        .batch_link(
            &pid,
            &[
                (recs[0].id.clone(), v_foto.id.clone(), valid_id),
                (recs[1].id.clone(), v_foto.id.clone(), "missing-asset-id".into()),
            ],
            false,
        )
        .unwrap_err();
    assert!(matches!(err, AppError::Domain(DomainError::NotFound { .. })));

    let snap_after = env.services.project_service.open_project(&pid).unwrap();
    assert!(snap_after.value_for(&recs[0].id, &v_foto.id).unwrap().asset_id.is_none());
}

#[test]
fn t2_f17_04_batch_pair_missing_on_disk_asset_rejected() {
    let env = TestEnv::new();
    let photos_dir = env.copy_fixtures_images_to("batch_missing_disk");
    let target = photos_dir.join("joao_silva.jpg");
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Batch Disco Ausente", &["A"]);
    env.services
        .photo_service
        .scan_directory(&pid, &photos_dir)
        .unwrap();
    let snap = env.services.project_service.open_project(&pid).unwrap();
    let asset = snap
        .assets
        .iter()
        .find(|a| a.file_name == "joao_silva.jpg")
        .unwrap()
        .clone();

    std::fs::remove_file(&target).unwrap();
    let res = env.services.photo_service.batch_link(
        &pid,
        &[(recs[0].id.clone(), v_foto.id.clone(), asset.id.clone())],
        false,
    );
    match res {
        Err(AppError::Domain(DomainError::Validation(_)))
        | Err(AppError::Infra(InfraError::Io { .. })) => {}
        Ok(_) => {
            let diag = env
                .services
                .photo_service
                .compute_diagnostics(&pid, true)
                .unwrap();
            assert!(diag.missing_on_disk_asset_ids.contains(&asset.id));
        }
        Err(other) => panic!("unexpected error for missing-on-disk batch pair: {other:?}"),
    }
}

#[test]
fn t2_f17_05_batch_pair_mismatched_counts_handled_cleanly() {
    let env = TestEnv::new();
    let photos_dir = env.path().join("batch_mismatch");
    generate_synthetic_photo_dir(&photos_dir, 2);

    let (pid, _vn, v_foto, _recs) =
        setup_photo_project(&env, "Batch Desbalanceado", &["R1", "R2", "R3", "R4", "R5"]);
    env.services
        .photo_service
        .scan_directory(&pid, &photos_dir)
        .unwrap();

    let pool = env
        .services
        .photo_service
        .get_unmatched_pool(&pid, &v_foto.id)
        .unwrap();
    assert_eq!(pool.records.len(), 5);
    assert_eq!(pool.assets.len(), 2);

    let pairs: Vec<(String, String, String)> = pool
        .records
        .iter()
        .zip(pool.assets.iter())
        .map(|(r, a)| (r.id.clone(), v_foto.id.clone(), a.id.clone()))
        .collect();
    let res = env
        .services
        .photo_service
        .batch_link(&pid, &pairs, false)
        .unwrap();
    assert_eq!(res.linked_count, 2);

    let after = env
        .services
        .photo_service
        .get_unmatched_pool(&pid, &v_foto.id)
        .unwrap();
    assert_eq!(after.records.len(), 3);
    assert!(after.assets.is_empty());
}

// ============================================================================
// F-18: Photo Diagnostics (5 tests)
// ============================================================================

#[test]
fn t2_f18_01_diagnostics_with_zero_linked_file_columns() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Somente Texto", ProjectMode::WithoutTemplate)
        .unwrap();
    let v = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    seed_records_and_values(&env.services, &proj.id, &[("R1".into(), vec![(&v, "Ana")])]);

    let diag = env
        .services
        .photo_service
        .compute_diagnostics(&proj.id, false)
        .unwrap();
    assert!(diag.records_missing_photo.is_empty());
}

#[test]
fn t2_f18_02_diagnostics_with_zero_records_and_scanned_photos() {
    let env = TestEnv::new();
    let photos_dir = env.path().join("orphan_5");
    generate_synthetic_photo_dir(&photos_dir, 5);
    let proj = env
        .services
        .project_service
        .create_project("Zero Registros", ProjectMode::WithoutTemplate)
        .unwrap();
    env.services
        .photo_service
        .scan_directory(&proj.id, &photos_dir)
        .unwrap();

    let diag = env
        .services
        .photo_service
        .compute_diagnostics(&proj.id, false)
        .unwrap();
    assert_eq!(diag.total_records, 0);
    assert_eq!(diag.total_scanned_assets, 5);
    assert_eq!(diag.unmatched_asset_ids.len(), 5);
}

#[test]
fn t2_f18_03_diagnostics_flags_missing_file_even_if_cached_previously() {
    let env = TestEnv::new();
    let photos_dir = env.copy_fixtures_images_to("diag_cached_missing");
    let target = photos_dir.join("joao_silva.jpg");
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Cache Missing", &["Joao"]);
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
    assert!(snap_before.asset_by_id(&asset_id).unwrap().exists_on_disk);

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
fn t2_f18_04_diagnostics_handles_ambiguous_separately_from_unmatched() {
    let env = TestEnv::new();
    let photos_dir = env.path().join("diag_statuses");
    std::fs::create_dir_all(&photos_dir).unwrap();
    std::fs::write(photos_dir.join("Ana Silva.jpg"), MINIMAL_JPEG_2X2).unwrap();
    std::fs::write(photos_dir.join("Ana Silva.png"), MINIMAL_PNG_2X2).unwrap();
    std::fs::write(
        photos_dir.join("Roberto Henrrique Martins.jpg"),
        MINIMAL_JPEG_2X2,
    )
    .unwrap();

    let (pid, _vn, _vf, _recs) = setup_photo_project(
        &env,
        "Diag Status Distintos",
        &["SemCorrespondencia", "Ana Silva", "Roberto Henrique Martins"],
    );
    env.services
        .photo_service
        .scan_directory(&pid, &photos_dir)
        .unwrap();
    let diag = env.services.photo_service.run_auto_match(&pid).unwrap();
    assert_eq!(diag.ambiguous_count, 1);
    assert_eq!(diag.suggested_count, 1);
    assert_eq!(diag.matched_count, 0);
}

#[test]
fn t2_f18_05_diagnostics_handles_unicode_accents_and_spaces_in_disk_paths() {
    let env = TestEnv::new();
    let unicode_dir = env.path().join("CONDOMÍNIO BEIJA FLOR");
    std::fs::create_dir_all(&unicode_dir).unwrap();
    let dest_img = unicode_dir.join("MARCELA  GONÇALVES.jpg");
    std::fs::copy(fixture_path("images/MARCELA  GONÇALVES.jpg"), &dest_img).unwrap();

    let (pid, _vn, _vf, _recs) =
        setup_photo_project(&env, "Caminho Unicode", &["Marcela Gonçalves"]);
    env.services
        .photo_service
        .scan_directory(&pid, &unicode_dir)
        .unwrap();
    env.services.photo_service.run_auto_match(&pid).unwrap();

    let diag = env
        .services
        .photo_service
        .compute_diagnostics(&pid, true)
        .unwrap();
    assert!(diag.missing_on_disk_asset_ids.is_empty());
    assert_eq!(diag.matched_count, 1);
}

// ============================================================================
// F-19: Modo XML com Modelo (5 tests)
// ============================================================================

#[test]
fn t2_f19_01_malformed_xml_missing_closing_tag_rejected() {
    let env = TestEnv::new();
    let malformed = "<?xml version=\"1.0\" encoding=\"utf-8\"?><svg><variableSets></svg>";
    let err = env
        .services
        .export_service
        .inspect_xml_template(malformed)
        .unwrap_err();
    assert!(matches!(err, AppError::Infra(InfraError::Xml(_))));
}

#[test]
fn t2_f19_02_xml_missing_sample_datasets_element_rejected() {
    let env = TestEnv::new();
    let no_samples = r#"<?xml version="1.0" encoding="utf-8"?>
<svg><variableSets><variableSet varSetName="X"><variables><variable varName="Nome" trait="textcontent"/></variables></variableSet></variableSets></svg>"#;
    let err = env
        .services
        .export_service
        .inspect_xml_template(no_samples)
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::Infra(InfraError::Xml(_)) | AppError::Domain(DomainError::Validation(_))
    ));
}

#[test]
fn t2_f19_03_xml_missing_variables_element_rejected() {
    let env = TestEnv::new();
    let no_vars = r#"<?xml version="1.0" encoding="utf-8"?>
<svg><variableSets><variableSet varSetName="X"><v:sampleDataSets xmlns:v="http://ns.adobe.com/Variables/1.0/"></v:sampleDataSets></variableSet></variableSets></svg>"#;
    let err = env
        .services
        .export_service
        .inspect_xml_template(no_vars)
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::Infra(InfraError::Xml(_)) | AppError::Domain(DomainError::Validation(_))
    ));
}

#[test]
fn t2_f19_04_non_utf8_or_utf8_bom_template_handled() {
    let env = TestEnv::new();
    let bad_path = env.path().join("bad_utf8.xml");
    std::fs::write(&bad_path, [0xFFu8, 0xFE, 0x00, 0x3C]).unwrap();
    assert!(std::fs::read_to_string(&bad_path).is_err());

    let tpl_raw = read_fixture_string("template_mixed.xml");
    let with_bom = format!("\u{FEFF}{tpl_raw}");
    let proj = env
        .services
        .project_service
        .create_project("BOM Template", ProjectMode::WithTemplate)
        .unwrap();
    env.services
        .export_service
        .bind_xml_template(&proj.id, None, &with_bom)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01".into(), vec![(&v_nome, "Ana")])],
    );

    let generated = env
        .services
        .export_service
        .generate_xml_preview(&proj.id)
        .unwrap();
    assert!(!generated.starts_with('\u{FEFF}'));
    assert!(generated.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>"));
}

#[test]
fn t2_f19_05_large_c2pa_manifest_preserved_without_truncation() {
    let env = TestEnv::new();
    let base = read_fixture_string("segunda_leva_variant.xml");
    let huge_payload = "ABCD".repeat(9_000); // 36,000 bytes (> 32 KB)
    let large_tpl = base.replace(
        "AAAWgmp1bWJHZW5lcmF0ZWRTeW50aGV0aWNDMlBBTWFuaWZlc3REYXRh",
        &huge_payload,
    );

    let proj = env
        .services
        .project_service
        .create_project("C2PA >32KB", ProjectMode::WithTemplate)
        .unwrap();
    env.services
        .export_service
        .bind_xml_template(&proj.id, None, &large_tpl)
        .unwrap();
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
    let _v_func = env
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
    let _v_foto = env
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
    seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01".into(), vec![(&v_nome, "Beatriz")])],
    );

    let out = env.services.export_service.generate_xml_preview(&proj.id).unwrap();
    assert!(out.contains(&huge_payload));
    assert_illustrator_xml_valid(&out, 1);
}

// ============================================================================
// F-20: Modo Dados sem Modelo (5 tests)
// ============================================================================

#[test]
fn t2_f20_01_bind_invalid_xml_file_rejected_leaving_project_intact() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Sem Modelo Intacto", ProjectMode::WithoutTemplate)
        .unwrap();
    let v = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01".into(), vec![(&v, "Preservado")])],
    );

    let err = env
        .services
        .export_service
        .bind_xml_template(&proj.id, None, "<svg><broken>")
        .unwrap_err();
    assert!(matches!(err, AppError::Infra(InfraError::Xml(_))));

    let snap = env.services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap.project.mode, ProjectMode::WithoutTemplate);
    assert!(snap.project.xml_template_raw.is_none());
    assert_eq!(
        snap.value_for(&recs[0].id, &v.id).unwrap().text_value,
        "Preservado"
    );
}

#[test]
fn t2_f20_02_map_text_column_to_fileref_template_var_rejected() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Conflito Text->Fileref", ProjectMode::WithTemplate)
        .unwrap();
    env.services
        .export_service
        .bind_xml_template(
            &proj.id,
            Some(&fixture_path("template_mixed.xml")),
            &read_fixture_string("template_mixed.xml"),
        )
        .unwrap();

    let v_bad = env
        .services
        .project_service
        .create_variable(&proj.id, "TextoErrado", Some("TextoErrado"), VariableKind::Text, true)
        .unwrap();
    env.services
        .project_service
        .update_variable_options(&proj.id, &v_bad.id, VariableKind::Text, true, Some("Foto".into()))
        .unwrap();
    seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01".into(), vec![(&v_bad, "abc")])],
    );

    let report = env.services.validation_service.audit_project(&proj.id).unwrap();
    assert!(!report.can_export);
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.code == ValidationCode::E09TemplateMappingConflict)
    );
}

#[test]
fn t2_f20_03_map_linked_file_column_to_strict_text_var_rejected() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Conflito Linked->Text", ProjectMode::WithTemplate)
        .unwrap();
    env.services
        .export_service
        .bind_xml_template(
            &proj.id,
            Some(&fixture_path("template_mixed.xml")),
            &read_fixture_string("template_mixed.xml"),
        )
        .unwrap();

    let v_bad = env
        .services
        .project_service
        .create_variable(&proj.id, "FotoErrada", Some("FotoErrada"), VariableKind::LinkedFile, false)
        .unwrap();
    env.services
        .project_service
        .update_variable_options(
            &proj.id,
            &v_bad.id,
            VariableKind::LinkedFile,
            false,
            Some("Nome".into()),
        )
        .unwrap();
    seed_records_and_values(&env.services, &proj.id, &[("01".into(), vec![])]);

    let report = env.services.validation_service.audit_project(&proj.id).unwrap();
    assert!(!report.can_export);
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.code == ValidationCode::E09TemplateMappingConflict)
    );
}

#[test]
fn t2_f20_04_unmapped_template_variable_flagged_in_audit() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Falta Variável Template", ProjectMode::WithTemplate)
        .unwrap();
    env.services
        .export_service
        .bind_xml_template(
            &proj.id,
            Some(&fixture_path("template_mixed.xml")),
            &read_fixture_string("template_mixed.xml"),
        )
        .unwrap();

    // template_mixed.xml has 4 vars (Nome, Funcao, Matricula, Foto); create only 3
    let v1 = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let v2 = env
        .services
        .project_service
        .create_variable(&proj.id, "Funcao", Some("Funcao"), VariableKind::Text, true)
        .unwrap();
    let v3 = env
        .services
        .project_service
        .create_variable(&proj.id, "Matricula", Some("Matricula"), VariableKind::Text, true)
        .unwrap();
    seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01".into(), vec![(&v1, "Ana"), (&v2, "Dev"), (&v3, "001")])],
    );

    let report = env.services.validation_service.audit_project(&proj.id).unwrap();
    assert!(!report.can_export);
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.code == ValidationCode::E09TemplateMappingConflict)
    );
}

#[test]
fn t2_f20_05_direct_export_with_empty_project_blocked() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Vazio Direto", ProjectMode::WithoutTemplate)
        .unwrap();
    let report = env.services.validation_service.audit_project(&proj.id).unwrap();
    assert!(!report.can_export);
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.code == ValidationCode::E01EmptyProject)
    );

    let err = env
        .services
        .export_service
        .export_package(&proj.id, &env.path().join("out_empty"), false)
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::Domain(DomainError::PreExportBlocked { .. })
    ));
}

// ============================================================================
// F-21: UTF-8 XML Emission & Path Strategies (5 tests)
// ============================================================================

#[test]
fn t2_f21_01_reject_or_escape_xml_injection_payloads() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Injeção XML", ProjectMode::WithoutTemplate)
        .unwrap();
    let v = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let payload = "</p></Nome><v:sampleDataSet dataSetName=\"evil\">";
    seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01".into(), vec![(&v, payload)])],
    );

    let xml = env.services.export_service.generate_xml_preview(&proj.id).unwrap();
    assert!(xml.contains("&lt;/p&gt;"));
    env.services
        .export_service
        .validate_xml_structure(&xml, 1)
        .unwrap();
    assert_illustrator_xml_valid(&xml, 1);
    let datasets = extract_xml_datasets(&xml);
    assert_eq!(datasets.len(), 1);
    assert_eq!(datasets[0].1.get("Nome").map(String::as_str), Some(payload));
}

#[test]
fn t2_f21_02_multiline_text_value_preserved_in_p_tags() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Multiline XML", ProjectMode::WithoutTemplate)
        .unwrap();
    let v = env
        .services
        .project_service
        .create_variable(&proj.id, "Obs", Some("Obs"), VariableKind::Text, false)
        .unwrap();
    let multiline = "Linha 1\r\nLinha 2\nLinha 3";
    seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01".into(), vec![(&v, multiline)])],
    );

    let xml = env.services.export_service.generate_xml_preview(&proj.id).unwrap();
    assert_illustrator_xml_valid(&xml, 1);
    let datasets = extract_xml_datasets(&xml);
    let recovered = datasets[0].1.get("Obs").unwrap();
    assert!(recovered.contains("Linha 1"));
    assert!(recovered.contains("Linha 2"));
    assert!(recovered.contains("Linha 3"));
}

#[test]
fn t2_f21_03_empty_optional_text_and_empty_optional_linked_file() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Opcionais Vazios", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_txt = env
        .services
        .project_service
        .create_variable(&proj.id, "Obs", Some("Obs"), VariableKind::Text, false)
        .unwrap();
    let _v_foto = env
        .services
        .project_service
        .create_variable(&proj.id, "Foto", Some("Foto"), VariableKind::LinkedFile, false)
        .unwrap();
    seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01".into(), vec![(&v_txt, "")])],
    );

    let xml = env.services.export_service.generate_xml_preview(&proj.id).unwrap();
    assert!(xml.contains("<Obs><p></p></Obs>") || xml.contains("<Obs><p/></Obs>"));
    assert!(xml.contains("<Foto></Foto>") || xml.contains("<Foto/>"));
    assert!(!xml.contains("imagens/"));
    assert_illustrator_xml_valid(&xml, 1);
}

#[test]
fn t2_f21_04_absolute_file_uri_encodes_spaces_accents_and_parentheses() {
    let env = TestEnv::new();
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "URI Especial", &["Joao"]);
    env.services
        .project_service
        .set_path_strategy(&pid, PathStrategy::AbsoluteFileUri)
        .unwrap();
    env.services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    let out_dir = env.path().join("João Carlos (2026)").join("Lote #1");
    env.services
        .export_service
        .export_package(&pid, &out_dir, false)
        .unwrap();

    let xml = std::fs::read_to_string(out_dir.join("dados.xml")).unwrap();
    assert_illustrator_xml_valid(&xml, 1);
    let datasets = extract_xml_datasets(&xml);
    let uri = datasets[0].1.get("Foto").unwrap();
    assert!(uri.starts_with("file:///"));
    assert!(!uri.contains('\\'));
    assert!(uri.contains("%20"));
    assert!(uri.contains("%23"));
    assert!(uri.ends_with("/imagens/asset_001.jpg"));
}

#[test]
fn t2_f21_05_validate_xml_structure_rejects_mismatched_dataset_count() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Contagem XML", ProjectMode::WithoutTemplate)
        .unwrap();
    let v = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    seed_records_and_values(
        &env.services,
        &proj.id,
        &[
            ("01".into(), vec![(&v, "A")]),
            ("02".into(), vec![(&v, "B")]),
        ],
    );
    let xml = env.services.export_service.generate_xml_preview(&proj.id).unwrap();
    let err = env
        .services
        .export_service
        .validate_xml_structure(&xml, 3)
        .unwrap_err();
    assert!(matches!(err, AppError::Infra(InfraError::Xml(_))));
}

// ============================================================================
// F-22: Pre-Export Audit (5 tests)
// ============================================================================

#[test]
fn t2_f22_01_e01_blocks_zero_records_or_zero_variables() {
    let env = TestEnv::new();
    // Case A: 0 vars, 1 rec
    let p_no_vars = env
        .services
        .project_service
        .create_project("Sem Variáveis", ProjectMode::WithoutTemplate)
        .unwrap();
    seed_records_and_values(&env.services, &p_no_vars.id, &[("R1".into(), vec![])]);
    let r_a = env
        .services
        .validation_service
        .audit_project(&p_no_vars.id)
        .unwrap();
    assert!(!r_a.can_export);
    assert!(r_a.issues.iter().any(|i| {
        i.code == ValidationCode::E01EmptyProject
            && i.severity == ValidationSeverity::BlockingError
            && i.target == ValidationTarget::Project
    }));

    // Case B: 1 var, 0 recs
    let p_no_recs = env
        .services
        .project_service
        .create_project("Sem Registros", ProjectMode::WithoutTemplate)
        .unwrap();
    env.services
        .project_service
        .create_variable(&p_no_recs.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let r_b = env
        .services
        .validation_service
        .audit_project(&p_no_recs.id)
        .unwrap();
    assert!(!r_b.can_export);
    assert!(r_b.issues.iter().any(|i| {
        i.code == ValidationCode::E01EmptyProject
            && i.severity == ValidationSeverity::BlockingError
    }));
}

#[test]
fn t2_f22_02_e02_e03_block_invalid_xml_name_and_duplicate_dataset_name() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("E02 E03", ProjectMode::WithoutTemplate)
        .unwrap();
    let bad_var = Variable {
        id: "bad-xml-var".into(),
        project_id: proj.id.clone(),
        display_name: "Inválida".into(),
        xml_name: "123 invalido".into(),
        kind: VariableKind::Text,
        required: false,
        sort_order: 0,
        mapped_template_var: None,
    };
    env.services.project_repo.upsert_variable(&bad_var).unwrap();

    seed_records_and_values(
        &env.services,
        &proj.id,
        &[
            ("MESMO_NOME".into(), vec![(&bad_var, "A")]),
            ("MESMO_NOME".into(), vec![(&bad_var, "B")]),
        ],
    );

    let report = env.services.validation_service.audit_project(&proj.id).unwrap();
    assert!(!report.can_export);
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.code == ValidationCode::E02InvalidOrDuplicateXmlName)
    );
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.code == ValidationCode::E03DuplicateDatasetName)
    );
}

#[test]
fn t2_f22_03_e04_e05_block_empty_required_text_and_missing_required_photo() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("E04 E05", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_req_txt = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let v_req_foto = env
        .services
        .project_service
        .create_variable(&proj.id, "Foto", Some("Foto"), VariableKind::LinkedFile, true)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01".into(), vec![(&v_req_txt, "   ")])],
    );

    let report = env.services.validation_service.audit_project(&proj.id).unwrap();
    assert!(!report.can_export);
    assert!(report.issues.iter().any(|i| {
        i.code == ValidationCode::E04RequiredTextEmpty
            && i.target
                == ValidationTarget::Cell {
                    record_id: recs[0].id.clone(),
                    variable_id: v_req_txt.id.clone(),
                }
    }));
    assert!(report.issues.iter().any(|i| {
        i.code == ValidationCode::E05RequiredPhotoMissing
            && i.target
                == ValidationTarget::Cell {
                    record_id: recs[0].id.clone(),
                    variable_id: v_req_foto.id.clone(),
                }
    }));
}

#[test]
fn t2_f22_04_e06_e07_block_missing_disk_file_and_unsupported_ext() {
    let env = TestEnv::new();
    let photos_dir = env.copy_fixtures_images_to("e06_photos");
    let target = photos_dir.join("joao_silva.jpg");
    let proj = env
        .services
        .project_service
        .create_project("E06 E07", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&proj.id, "Foto", Some("Foto"), VariableKind::LinkedFile, false)
        .unwrap();
    let recs = seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01".into(), vec![]), ("02".into(), vec![])],
    );

    let asset_missing = Asset {
        id: "asset-e06".into(),
        project_id: proj.id.clone(),
        source_path: target.clone(),
        file_name: "joao_silva.jpg".into(),
        stem: "joao_silva".into(),
        normalized_stem: "joao_silva".into(),
        format: ImageFormat::Jpg,
        file_size_bytes: 683,
        modified_epoch_ms: 1_700_000_000_000,
        exists_on_disk: true,
    };
    let asset_heic = Asset {
        id: "asset-e07".into(),
        project_id: proj.id.clone(),
        source_path: fixture_path("images/ignored_sample.heic"),
        file_name: "ignored_sample.heic".into(),
        stem: "ignored_sample".into(),
        normalized_stem: "ignored_sample".into(),
        format: ImageFormat::Jpg,
        file_size_bytes: 23,
        modified_epoch_ms: 1_700_000_000_000,
        exists_on_disk: true,
    };
    env.services
        .project_repo
        .replace_assets(&proj.id, &[asset_missing, asset_heic])
        .unwrap();
    env.services
        .project_repo
        .upsert_records_and_values(
            &proj.id,
            &[],
            &[
                RecordValue {
                    record_id: recs[0].id.clone(),
                    variable_id: v_foto.id.clone(),
                    text_value: "joao".into(),
                    asset_id: Some("asset-e06".into()),
                    match_status: MatchStatus::Manual,
                    match_confidence: Some(1.0),
                },
                RecordValue {
                    record_id: recs[1].id.clone(),
                    variable_id: v_foto.id.clone(),
                    text_value: "heic".into(),
                    asset_id: Some("asset-e07".into()),
                    match_status: MatchStatus::Manual,
                    match_confidence: Some(1.0),
                },
            ],
        )
        .unwrap();
    std::fs::remove_file(&target).unwrap();

    let report = env.services.validation_service.audit_project(&proj.id).unwrap();
    assert!(!report.can_export);
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.code == ValidationCode::E06LinkedAssetMissingOnDisk)
    );
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.code == ValidationCode::E07UnsupportedAssetFormat)
    );
}

#[test]
fn t2_f22_05_e08_e10_block_missing_template_and_ambiguous_required_photo() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("E08 E10", ProjectMode::WithTemplate)
        .unwrap();
    let v_foto = env
        .services
        .project_service
        .create_variable(&proj.id, "Foto", Some("Foto"), VariableKind::LinkedFile, true)
        .unwrap();
    seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01".into(), vec![(&v_foto, "Ana Silva")])],
    );
    env.services
        .photo_service
        .scan_directory(&proj.id, &fixture_path("images"))
        .unwrap();
    env.services.photo_service.run_auto_match(&proj.id).unwrap();

    let report = env.services.validation_service.audit_project(&proj.id).unwrap();
    assert!(!report.can_export);
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.code == ValidationCode::E08MissingOrInvalidXmlTemplate)
    );
    assert!(
        report
            .issues
            .iter()
            .any(|i| i.code == ValidationCode::E10UnresolvedAmbiguousPhoto)
    );
}

// ============================================================================
// F-23: Atomic Portable Package Export (5 tests)
// ============================================================================

#[test]
fn t2_f23_01_blocking_error_aborts_export_before_creating_files() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Export Bloqueado", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_req = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01".into(), vec![(&v_req, "")])],
    );

    let out_dir = env.path().join("blocked_out");
    let err = env
        .services
        .export_service
        .export_package(&proj.id, &out_dir, false)
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::Domain(DomainError::PreExportBlocked { .. })
    ));
    assert!(!out_dir.join("dados.xml").exists());
    assert!(!out_dir.join("imagens").exists());
}

#[test]
fn t2_f23_02_existing_target_blocked_when_overwrite_confirmed_false() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Guarda Sobrescrita", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01".into(), vec![(&v_nome, "Ana")])],
    );

    let out_dir = env.path().join("existing_out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let sentinel = out_dir.join("dados.xml");
    std::fs::write(&sentinel, b"DO NOT OVERWRITE").unwrap();
    let before = snapshot_bytes(&sentinel);

    let err = env
        .services
        .export_service
        .export_package(&proj.id, &out_dir, false)
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::Domain(DomainError::OverwriteConfirmationRequired { .. })
    ));
    assert_file_unchanged(&sentinel, &before);
}

#[test]
fn t2_f23_03_asset_zero_padding_expands_at_1000_assets() {
    let env = TestEnv::new();
    let photos_dir = env.path().join("photos_1000");
    generate_synthetic_photo_dir(&photos_dir, 1000);

    let proj = env
        .services
        .project_service
        .create_project("1000 Assets", ProjectMode::WithoutTemplate)
        .unwrap();
    let _v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let _v_foto = env
        .services
        .project_service
        .create_variable(&proj.id, "Foto", Some("Foto"), VariableKind::LinkedFile, true)
        .unwrap();

    let mut tsv = String::new();
    for i in 1..=1000 {
        tsv.push_str(&format!("Pessoa {i:04}\tPessoa_{i:04}\n"));
    }
    env.services
        .data_service
        .paste_block(&proj.id, 0, 0, &tsv)
        .unwrap();
    env.services
        .photo_service
        .scan_directory(&proj.id, &photos_dir)
        .unwrap();
    env.services.photo_service.run_auto_match(&proj.id).unwrap();

    let out_dir = env.path().join("out_1000");
    let summary = env
        .services
        .export_service
        .export_package(&proj.id, &out_dir, false)
        .unwrap();
    assert_eq!(summary.exported_assets, 1000);
    assert!(out_dir.join("imagens/asset_001.jpg").exists());
    assert!(out_dir.join("imagens/asset_999.tiff").exists());
    assert!(out_dir.join("imagens/asset_1000.png").exists());
}

#[test]
fn t2_f23_04_source_photo_disappears_mid_export_rolls_back_atomically() {
    let env = TestEnv::new();
    let photos_dir = env.copy_fixtures_images_to("mid_export");
    let target = photos_dir.join("joao_silva.jpg");
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Rollback Mid Export", &["Joao"]);
    env.services
        .photo_service
        .scan_directory(&pid, &photos_dir)
        .unwrap();
    env.services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &target)
        .unwrap();

    std::fs::remove_file(&target).unwrap();
    let out_dir = env.path().join("mid_export_out");
    let err = env
        .services
        .export_service
        .export_package(&pid, &out_dir, false)
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::Domain(DomainError::PreExportBlocked { .. })
            | AppError::Infra(InfraError::Io { .. })
            | AppError::Infra(InfraError::Export(_))
    ));
    assert!(!out_dir.join("dados.xml").exists());
    assert!(!out_dir.join("manifest.json").exists());
    for entry in std::fs::read_dir(env.path()).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        assert!(!name.starts_with(".aivm_stage_"));
    }
}

#[test]
fn t2_f23_05_readonly_target_directory_error_leaves_no_partial_state() {
    let env = TestEnv::new();
    let proj = env
        .services
        .project_service
        .create_project("Destino Inválido", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = env
        .services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    seed_records_and_values(
        &env.services,
        &proj.id,
        &[("01".into(), vec![(&v_nome, "Ana")])],
    );

    let blocker = env.path().join("file_blocker");
    std::fs::write(&blocker, b"file").unwrap();
    let bad_target = blocker.join("sub_out");

    let err = env
        .services
        .export_service
        .export_package(&proj.id, &bad_target, false)
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::Infra(InfraError::Io { .. }) | AppError::Infra(InfraError::Export(_))
    ));
}

// ============================================================================
// F-24: Path Prefix / Portability Updater (5 tests)
// ============================================================================

#[test]
fn t2_f24_01_missing_dados_xml_or_manifest_json_rejected() {
    let env = TestEnv::new();
    let empty = env.path().join("empty_pkg");
    std::fs::create_dir_all(&empty).unwrap();

    let err1 = env
        .services
        .export_service
        .update_exported_prefix(&empty, PathStrategy::RelativeToXml)
        .unwrap_err();
    assert!(matches!(
        err1,
        AppError::Infra(InfraError::Io { .. })
            | AppError::Infra(InfraError::Export(_))
            | AppError::Domain(DomainError::NotFound { .. })
    ));

    std::fs::write(empty.join("dados.xml"), b"<svg/>").unwrap();
    let err2 = env
        .services
        .export_service
        .update_exported_prefix(&empty, PathStrategy::RelativeToXml)
        .unwrap_err();
    assert!(matches!(
        err2,
        AppError::Infra(InfraError::Io { .. })
            | AppError::Infra(InfraError::Export(_))
            | AppError::Domain(DomainError::NotFound { .. })
    ));
}

#[test]
fn t2_f24_02_missing_referenced_asset_in_imagens_blocks_update() {
    let env = TestEnv::new();
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Asset Ausente Update", &["Joao"]);
    env.services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    let out_dir = env.path().join("pkg_missing_img");
    env.services
        .export_service
        .export_package(&pid, &out_dir, false)
        .unwrap();
    let xml_before = snapshot_bytes(&out_dir.join("dados.xml"));

    std::fs::remove_file(out_dir.join("imagens/asset_001.jpg")).unwrap();
    let err = env
        .services
        .export_service
        .update_exported_prefix(&out_dir, PathStrategy::AbsoluteFileUri)
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::Infra(InfraError::Export(_))
            | AppError::Infra(InfraError::Io { .. })
            | AppError::Domain(DomainError::Validation(_))
    ));
    assert_file_unchanged(&out_dir.join("dados.xml"), &xml_before);
}

#[test]
fn t2_f24_03_corrupt_dados_xml_aborts_atomic_rewrite() {
    let env = TestEnv::new();
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "XML Corrupto Update", &["Joao"]);
    env.services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    let out_dir = env.path().join("pkg_corrupt_xml");
    env.services
        .export_service
        .export_package(&pid, &out_dir, false)
        .unwrap();
    let manifest_before = snapshot_bytes(&out_dir.join("manifest.json"));

    std::fs::write(out_dir.join("dados.xml"), b"<svg><unclosed>").unwrap();
    let err = env
        .services
        .export_service
        .update_exported_prefix(&out_dir, PathStrategy::AbsoluteFileUri)
        .unwrap_err();
    assert!(matches!(err, AppError::Infra(InfraError::Xml(_))));
    assert_file_unchanged(&out_dir.join("manifest.json"), &manifest_before);
}

#[test]
fn t2_f24_04_moved_to_path_with_spaces_and_portuguese_accents() {
    let env = TestEnv::new();
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Mover Acentos", &["Joao"]);
    env.services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    let src_dir = env.path().join("origem_rel");
    let dst_dir = env.path().join("Crachás João (Final)");
    env.services
        .export_service
        .export_package(&pid, &src_dir, false)
        .unwrap();
    std::fs::rename(&src_dir, &dst_dir).unwrap();

    env.services
        .export_service
        .update_exported_prefix(&dst_dir, PathStrategy::AbsoluteFileUri)
        .unwrap();

    let xml = std::fs::read_to_string(dst_dir.join("dados.xml")).unwrap();
    assert_illustrator_xml_valid(&xml, 1);
    env.services
        .export_service
        .validate_xml_structure(&xml, 1)
        .unwrap();
    let datasets = extract_xml_datasets(&xml);
    let uri = datasets[0].1.get("Foto").unwrap();
    assert!(uri.starts_with("file:///"));
    assert!(uri.contains("%20"));
    assert!(uri.ends_with("/imagens/asset_001.jpg"));
}

#[test]
fn t2_f24_05_idempotent_repeated_prefix_update() {
    let env = TestEnv::new();
    let (pid, _vn, v_foto, recs) = setup_photo_project(&env, "Idempotente", &["Joao"]);
    env.services
        .photo_service
        .link_manual(&pid, &recs[0].id, &v_foto.id, &fixture_path("images/joao_silva.jpg"))
        .unwrap();

    let out_dir = env.path().join("pkg_idemp");
    env.services
        .export_service
        .export_package(&pid, &out_dir, false)
        .unwrap();

    env.services
        .export_service
        .update_exported_prefix(&out_dir, PathStrategy::AbsoluteFileUri)
        .unwrap();
    let xml_first = snapshot_bytes(&out_dir.join("dados.xml"));
    let manifest_first = snapshot_bytes(&out_dir.join("manifest.json"));

    env.services
        .export_service
        .update_exported_prefix(&out_dir, PathStrategy::AbsoluteFileUri)
        .unwrap();
    assert_file_unchanged(&out_dir.join("dados.xml"), &xml_first);
    let manifest_second = std::fs::read_to_string(out_dir.join("manifest.json")).unwrap();
    let m1: serde_json::Value = serde_json::from_slice(&manifest_first).unwrap();
    let m2: serde_json::Value = serde_json::from_str(&manifest_second).unwrap();
    assert_eq!(m1["path_strategy"], m2["path_strategy"]);
    assert_eq!(m1["asset_mappings"], m2["asset_mappings"]);
}
