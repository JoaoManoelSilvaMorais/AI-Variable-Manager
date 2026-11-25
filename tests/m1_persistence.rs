use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Condvar, Mutex};

use ai_variable_manager::AppServices;
use ai_variable_manager::application::ProjectService;
use ai_variable_manager::domain::{
    Asset, ImageFormat, MatchCandidate, MatchStatus, PathStrategy, Project, ProjectMode,
    ProjectRepository, ProjectSnapshot, ProjectSummary, Record, RecordValue, ThemePreference,
    Variable, VariableKind,
};
use ai_variable_manager::shared::error::{AppError, AppResult, DomainError};
use tempfile::TempDir;

#[test]
fn project_lifecycle_create_open_reopen_rename_delete() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("aivm.db");

    let (id1, id2) = {
        let services = AppServices::new_with_db_path(&db_path).unwrap();
        let p1 = services
            .project_service
            .create_project("Crachás SECOM", ProjectMode::WithTemplate)
            .unwrap();
        let p2 = services
            .project_service
            .create_project("Cadastro Livre", ProjectMode::WithoutTemplate)
            .unwrap();

        let list = services.project_service.list_projects().unwrap();
        assert_eq!(list.len(), 2);
        for summary in &list {
            assert_eq!(summary.variable_count, 0);
            assert_eq!(summary.record_count, 0);
            assert_eq!(summary.asset_count, 0);
        }

        services
            .project_service
            .rename_project(&p1.id, "Crachás SECOM 2026")
            .unwrap();
        let opened = services.project_service.open_project(&p1.id).unwrap();
        assert_eq!(opened.project.name, "Crachás SECOM 2026");
        assert_eq!(opened.project.mode, ProjectMode::WithTemplate);
        assert_eq!(opened.project.path_strategy, PathStrategy::AbsolutePath);

        services
            .project_service
            .set_theme_preference(ThemePreference::Dark)
            .unwrap();

        assert_eq!(
            services
                .project_service
                .get_last_opened_project_id()
                .unwrap(),
            Some(p1.id.clone())
        );

        (p1.id, p2.id)
    };

    // Reabrir o mesmo banco em uma nova instância de AppServices
    let services = AppServices::new_with_db_path(&db_path).unwrap();
    assert_eq!(
        services.project_service.get_theme_preference().unwrap(),
        ThemePreference::Dark
    );
    let reopened = services.project_service.open_project(&id1).unwrap();
    assert_eq!(reopened.project.name, "Crachás SECOM 2026");
    assert_eq!(reopened.project.mode, ProjectMode::WithTemplate);
    assert_eq!(
        reopened.project.path_strategy,
        PathStrategy::AbsolutePath
    );

    services.project_service.delete_project(&id1).unwrap();
    let remaining = services.project_service.list_projects().unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, id2);

    let err = services.project_service.open_project(&id1).unwrap_err();
    assert!(matches!(err, AppError::Domain(DomainError::NotFound { .. })));
}

#[test]
fn dynamic_columns_crud_duplicate_reorder_zero_ddl() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("aivm.db");
    let services = AppServices::new_with_db_path(&db_path).unwrap();

    let proj = services
        .project_service
        .create_project("Colunas Dinâmicas", ProjectMode::WithoutTemplate)
        .unwrap();

    let v_nome = services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();
    let v_mat = services
        .project_service
        .create_variable(
            &proj.id,
            "Matrícula",
            Some("Matricula"),
            VariableKind::Text,
            true,
        )
        .unwrap();
    let v_foto = services
        .project_service
        .create_variable(
            &proj.id,
            "Foto",
            Some("Foto"),
            VariableKind::LinkedFile,
            false,
        )
        .unwrap();

    let records: Vec<Record> = (0..3)
        .map(|i| Record {
            id: format!("rec-{i}"),
            project_id: proj.id.clone(),
            dataset_name: format!("DS_{i}"),
            sort_order: i,
            created_at: "2026-11-17T00:00:00.000Z".into(),
            updated_at: "2026-11-17T00:00:00.000Z".into(),
        })
        .collect();
    let values = vec![
        RecordValue::new_text("rec-0", &v_nome.id, "Ana"),
        RecordValue::new_text("rec-0", &v_mat.id, "00421"),
        RecordValue::new_text("rec-1", &v_nome.id, "Bruno"),
        RecordValue::new_text("rec-1", &v_mat.id, "00007"),
        RecordValue::new_text("rec-2", &v_nome.id, "Carla"),
        RecordValue::new_text("rec-2", &v_mat.id, "00000"),
    ];
    services
        .project_repo
        .upsert_records_and_values(&proj.id, &records, &values)
        .unwrap();

    let table_info_before = read_table_columns(&db_path, "record_values");

    // Renomear Matrícula -> Registro (preservando valores EAV)
    let v_reg = services
        .project_service
        .rename_variable(&proj.id, &v_mat.id, "Registro", Some("Registro"))
        .unwrap();
    assert_eq!(v_reg.display_name, "Registro");
    assert_eq!(v_reg.xml_name, "Registro");

    // Duplicar Registro -> Registro_copia adjacente com valores copiados
    let v_dup = services
        .project_service
        .duplicate_variable(&proj.id, &v_reg.id)
        .unwrap();
    assert_eq!(v_dup.xml_name, "Registro_copia");
    assert_eq!(v_dup.display_name, "Registro (Cópia)");
    assert_eq!(v_dup.sort_order, v_reg.sort_order + 1);
    assert_eq!(v_dup.mapped_template_var, None);

    let snap_after_dup = services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(
        snap_after_dup.value_for("rec-0", &v_dup.id).unwrap().text_value,
        "00421"
    );
    assert_eq!(
        snap_after_dup.value_for("rec-1", &v_dup.id).unwrap().text_value,
        "00007"
    );
    assert_eq!(
        snap_after_dup.value_for("rec-2", &v_dup.id).unwrap().text_value,
        "00000"
    );

    // Reordenar [Foto, Registro_copia, Registro, Nome]
    services
        .project_service
        .reorder_variables(
            &proj.id,
            &[
                v_foto.id.clone(),
                v_dup.id.clone(),
                v_reg.id.clone(),
                v_nome.id.clone(),
            ],
        )
        .unwrap();

    let snap_reordered = services.project_service.open_project(&proj.id).unwrap();
    let ordered: Vec<(&str, usize)> = snap_reordered
        .variables
        .iter()
        .map(|v| (v.xml_name.as_str(), v.sort_order))
        .collect();
    assert_eq!(
        ordered,
        vec![
            ("Foto", 0),
            ("Registro_copia", 1),
            ("Registro", 2),
            ("Nome", 3)
        ]
    );

    // Excluir coluna duplicada e verificar compactação 0..3
    services
        .project_service
        .delete_variable(&proj.id, &v_dup.id)
        .unwrap();
    let snap_after_del = services.project_service.open_project(&proj.id).unwrap();
    let remaining_orders: Vec<(&str, usize)> = snap_after_del
        .variables
        .iter()
        .map(|v| (v.xml_name.as_str(), v.sort_order))
        .collect();
    assert_eq!(
        remaining_orders,
        vec![("Foto", 0), ("Registro", 1), ("Nome", 2)]
    );
    assert_eq!(
        snap_after_del.value_for("rec-0", &v_reg.id).unwrap().text_value,
        "00421"
    );

    let table_info_after = read_table_columns(&db_path, "record_values");
    assert_eq!(
        table_info_before, table_info_after,
        "Nenhuma DDL deve alterar o schema de record_values"
    );
}

#[test]
fn eav_leading_zero_preservation_and_unicode() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("aivm.db");
    let proj_id;
    let var_id;

    {
        let services = AppServices::new_with_db_path(&db_path).unwrap();
        let proj = services
            .project_service
            .create_project("Texto Rigoroso", ProjectMode::WithoutTemplate)
            .unwrap();
        let var = services
            .project_service
            .create_variable(&proj.id, "Valor", None, VariableKind::Text, false)
            .unwrap();

        let samples = [
            "00421",
            "0000",
            "+55 (62) 00999-0001",
            "José Ávila — Crachá",
            "",
        ];
        let records: Vec<Record> = samples
            .iter()
            .enumerate()
            .map(|(i, _)| Record {
                id: format!("r-{i}"),
                project_id: proj.id.clone(),
                dataset_name: format!("DS_{i}"),
                sort_order: i,
                created_at: "2026-11-17T00:00:00.000Z".into(),
                updated_at: "2026-11-17T00:00:00.000Z".into(),
            })
            .collect();
        let values: Vec<RecordValue> = samples
            .iter()
            .enumerate()
            .map(|(i, s)| RecordValue::new_text(format!("r-{i}"), &var.id, *s))
            .collect();

        services
            .project_repo
            .upsert_records_and_values(&proj.id, &records, &values)
            .unwrap();
        proj_id = proj.id;
        var_id = var.id;
    }

    let services = AppServices::new_with_db_path(&db_path).unwrap();
    let snap = services.project_service.open_project(&proj_id).unwrap();
    assert_eq!(snap.value_for("r-0", &var_id).unwrap().text_value, "00421");
    assert_eq!(snap.value_for("r-1", &var_id).unwrap().text_value, "0000");
    assert_eq!(
        snap.value_for("r-2", &var_id).unwrap().text_value,
        "+55 (62) 00999-0001"
    );
    assert_eq!(
        snap.value_for("r-3", &var_id).unwrap().text_value,
        "José Ávila — Crachá"
    );
    assert_eq!(snap.value_for("r-4", &var_id).unwrap().text_value, "");
}

#[test]
fn cascade_delete_cleans_all_child_tables() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("aivm.db");
    let services = AppServices::new_with_db_path(&db_path).unwrap();

    let proj = services
        .project_service
        .create_project("Cascata", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_foto = services
        .project_service
        .create_variable(&proj.id, "Foto", None, VariableKind::LinkedFile, true)
        .unwrap();

    let rec = Record {
        id: "rec-1".into(),
        project_id: proj.id.clone(),
        dataset_name: "DS_1".into(),
        sort_order: 0,
        created_at: "2026-11-17T00:00:00.000Z".into(),
        updated_at: "2026-11-17T00:00:00.000Z".into(),
    };
    let asset = Asset {
        id: "asset-1".into(),
        project_id: proj.id.clone(),
        source_path: PathBuf::from(r"C:\fotos\ana.jpg"),
        file_name: "ana.jpg".into(),
        stem: "ana".into(),
        normalized_stem: "ana".into(),
        format: ImageFormat::Jpg,
        file_size_bytes: 2048,
        modified_epoch_ms: 1_700_000_000_000,
        exists_on_disk: true,
    };
    services
        .project_repo
        .replace_assets(&proj.id, &[asset])
        .unwrap();

    let val = RecordValue {
        record_id: "rec-1".into(),
        variable_id: v_foto.id.clone(),
        text_value: "ana".into(),
        asset_id: Some("asset-1".into()),
        match_status: MatchStatus::AutoExact,
        match_confidence: Some(1.0),
    };
    services
        .project_repo
        .upsert_records_and_values(&proj.id, &[rec], &[val])
        .unwrap();
    services
        .project_repo
        .update_match_candidates(
            &proj.id,
            &[MatchCandidate {
                record_id: "rec-1".into(),
                variable_id: v_foto.id,
                asset_id: "asset-1".into(),
                score: 1.0,
                reason: "exact_normalized".into(),
            }],
        )
        .unwrap();

    let summaries = services.project_service.list_projects().unwrap();
    assert_eq!(summaries[0].asset_count, 1);

    services.project_service.delete_project(&proj.id).unwrap();

    let raw_conn = rusqlite::Connection::open(&db_path).unwrap();
    for table in [
        "projects",
        "variables",
        "records",
        "assets",
        "record_values",
        "photo_match_candidates",
    ] {
        let count: i64 = raw_conn
            .query_row(&format!("SELECT COUNT(*) FROM {table};"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "Tabela {table} deveria estar vazia após exclusão");
    }
}

#[test]
fn atomic_rollback_on_constraint_violation() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("aivm.db");
    let services = AppServices::new_with_db_path(&db_path).unwrap();

    let proj = services
        .project_service
        .create_project("Atomicidade", ProjectMode::WithoutTemplate)
        .unwrap();
    let v1 = services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();

    // Subcaso A: duplicidade de xml_name via serviço e via repositório direto
    let err_service = services
        .project_service
        .create_variable(&proj.id, "Outro", Some("Nome"), VariableKind::Text, false)
        .unwrap_err();
    assert!(matches!(
        err_service,
        AppError::Domain(DomainError::Conflict(_))
    ));

    let dup_var = Variable {
        id: "v-dup".into(),
        project_id: proj.id.clone(),
        display_name: "Outro".into(),
        xml_name: "Nome".into(),
        kind: VariableKind::Text,
        required: false,
        sort_order: 1,
        mapped_template_var: None,
    };
    let err_repo = services.project_repo.upsert_variable(&dup_var).unwrap_err();
    assert!(matches!(
        err_repo,
        AppError::Domain(DomainError::Conflict(_))
    ));

    let snap_a = services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap_a.variables.len(), 1);
    assert_eq!(snap_a.variables[0].id, v1.id);

    // Subcaso B: lote com 2 registros onde o segundo viola UNIQUE(project_id, sort_order)
    let r_ok = Record {
        id: "r-ok".into(),
        project_id: proj.id.clone(),
        dataset_name: "DS_10".into(),
        sort_order: 10,
        created_at: "2026-11-17T00:00:00.000Z".into(),
        updated_at: "2026-11-17T00:00:00.000Z".into(),
    };
    let r_bad = Record {
        id: "r-bad".into(),
        project_id: proj.id.clone(),
        dataset_name: "DS_10_dup".into(),
        sort_order: 10,
        created_at: "2026-11-17T00:00:00.000Z".into(),
        updated_at: "2026-11-17T00:00:00.000Z".into(),
    };
    let val_ok = RecordValue::new_text("r-ok", &v1.id, "Não deve persistir");

    let batch_err = services
        .project_repo
        .upsert_records_and_values(&proj.id, &[r_ok, r_bad], &[val_ok])
        .unwrap_err();
    assert!(matches!(
        batch_err,
        AppError::Domain(DomainError::Conflict(_))
    ));

    let snap_b = services.project_service.open_project(&proj.id).unwrap();
    assert!(
        snap_b.records.is_empty(),
        "Rollback atômico deve impedir persistência parcial de r-ok"
    );
    assert!(snap_b.values.is_empty());
}

#[test]
fn all_application_services_initialized() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("aivm.db");
    let services = AppServices::new_with_db_path(&db_path).unwrap();

    // Verify missing project returns clean NotFound rather than NotImplemented across all services
    assert!(matches!(
        services.data_service.get_records_window("missing-p", 0, 50),
        Err(AppError::Domain(DomainError::NotFound { .. }))
    ));
    assert!(matches!(
        services.photo_service.compute_diagnostics("missing-p", false),
        Err(AppError::Domain(DomainError::NotFound { .. }))
    ));
    assert!(matches!(
        services.validation_service.audit_project("missing-p"),
        Err(AppError::Domain(DomainError::NotFound { .. }))
    ));
    assert!(matches!(
        services.export_service.generate_xml_preview("missing-p"),
        Err(AppError::Domain(DomainError::NotFound { .. }))
    ));
}

#[test]
fn project_name_boundary_120_vs_121_and_255_via_service() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("aivm.db");
    let services = AppServices::new_with_db_path(&db_path).unwrap();

    let name_120 = "é".repeat(120);
    let created = services
        .project_service
        .create_project(&format!("  {name_120}  "), ProjectMode::WithoutTemplate)
        .unwrap();
    assert_eq!(created.name, name_120);

    let name_121 = "é".repeat(121);
    let err_121 = services
        .project_service
        .create_project(&name_121, ProjectMode::WithoutTemplate)
        .unwrap_err();
    assert!(matches!(err_121, AppError::Domain(DomainError::Validation(_))));

    let name_255 = "a".repeat(255);
    let err_255 = services
        .project_service
        .rename_project(&created.id, &name_255)
        .unwrap_err();
    assert!(matches!(err_255, AppError::Domain(DomainError::Validation(_))));

    let err_empty = services
        .project_service
        .rename_project(&created.id, "   \t  ")
        .unwrap_err();
    assert!(matches!(
        err_empty,
        AppError::Domain(DomainError::Validation(_))
    ));
}

#[test]
fn reorder_records_persists_new_order_and_rejects_invalid_permutation() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("aivm.db");
    let services = AppServices::new_with_db_path(&db_path).unwrap();

    let proj = services
        .project_service
        .create_project("Ordem Registros", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_nome = services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();

    let records: Vec<Record> = (0..3)
        .map(|i| Record {
            id: format!("rec-{i}"),
            project_id: proj.id.clone(),
            dataset_name: format!("DS_{i}"),
            sort_order: i,
            created_at: "2026-11-17T00:00:00.000Z".into(),
            updated_at: "2026-11-17T00:00:00.000Z".into(),
        })
        .collect();
    let values = vec![
        RecordValue::new_text("rec-0", &v_nome.id, "Primeiro"),
        RecordValue::new_text("rec-1", &v_nome.id, "Segundo"),
        RecordValue::new_text("rec-2", &v_nome.id, "Terceiro"),
    ];
    services
        .project_repo
        .upsert_records_and_values(&proj.id, &records, &values)
        .unwrap();

    services
        .project_repo
        .reorder_records(&proj.id, &["rec-2".into(), "rec-0".into(), "rec-1".into()])
        .unwrap();

    let snap = services.project_service.open_project(&proj.id).unwrap();
    let ids: Vec<(&str, usize)> = snap
        .records
        .iter()
        .map(|r| (r.id.as_str(), r.sort_order))
        .collect();
    assert_eq!(ids, vec![("rec-2", 0), ("rec-0", 1), ("rec-1", 2)]);
    assert_eq!(snap.value_for("rec-2", &v_nome.id).unwrap().text_value, "Terceiro");

    let err = services
        .project_repo
        .reorder_records(&proj.id, &["rec-0".into(), "rec-0".into(), "rec-1".into()])
        .unwrap_err();
    assert!(matches!(err, AppError::Domain(DomainError::Validation(_))));
    let snap_after_err = services.project_service.open_project(&proj.id).unwrap();
    let ids_after_err: Vec<&str> = snap_after_err
        .records
        .iter()
        .map(|r| r.id.as_str())
        .collect();
    assert_eq!(ids_after_err, vec!["rec-2", "rec-0", "rec-1"]);
}

#[test]
fn update_variable_options_kind_switch_clears_stale_asset_links_and_candidates() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("aivm.db");
    let services = AppServices::new_with_db_path(&db_path).unwrap();

    let proj = services
        .project_service
        .create_project("Alternar Tipo", ProjectMode::WithoutTemplate)
        .unwrap();
    let v_photo = services
        .project_service
        .create_variable(&proj.id, "Foto", Some("Foto"), VariableKind::LinkedFile, true)
        .unwrap();
    let v_other = services
        .project_service
        .create_variable(
            &proj.id,
            "FotoSec",
            Some("FotoSec"),
            VariableKind::LinkedFile,
            false,
        )
        .unwrap();

    let asset = Asset {
        id: "asset-1".into(),
        project_id: proj.id.clone(),
        source_path: PathBuf::from(r"C:\fotos\ana.jpg"),
        file_name: "ana.jpg".into(),
        stem: "ana".into(),
        normalized_stem: "ana".into(),
        format: ImageFormat::Jpg,
        file_size_bytes: 1024,
        modified_epoch_ms: 1_700_000_000_000,
        exists_on_disk: true,
    };
    services
        .project_repo
        .replace_assets(&proj.id, &[asset])
        .unwrap();

    let rec = Record {
        id: "rec-1".into(),
        project_id: proj.id.clone(),
        dataset_name: "DS_1".into(),
        sort_order: 0,
        created_at: "2026-11-17T00:00:00.000Z".into(),
        updated_at: "2026-11-17T00:00:00.000Z".into(),
    };
    let val_photo = RecordValue {
        record_id: "rec-1".into(),
        variable_id: v_photo.id.clone(),
        text_value: "00421".into(),
        asset_id: Some("asset-1".into()),
        match_status: MatchStatus::AutoExact,
        match_confidence: Some(0.99),
    };
    let val_other = RecordValue {
        record_id: "rec-1".into(),
        variable_id: v_other.id.clone(),
        text_value: "ana".into(),
        asset_id: Some("asset-1".into()),
        match_status: MatchStatus::Manual,
        match_confidence: Some(1.0),
    };
    services
        .project_repo
        .upsert_records_and_values(&proj.id, &[rec], &[val_photo, val_other])
        .unwrap();
    services
        .project_repo
        .update_match_candidates(
            &proj.id,
            &[
                MatchCandidate {
                    record_id: "rec-1".into(),
                    variable_id: v_photo.id.clone(),
                    asset_id: "asset-1".into(),
                    score: 0.99,
                    reason: "exact_normalized".into(),
                },
                MatchCandidate {
                    record_id: "rec-1".into(),
                    variable_id: v_other.id.clone(),
                    asset_id: "asset-1".into(),
                    score: 1.0,
                    reason: "manual".into(),
                },
            ],
        )
        .unwrap();

    // Atualizar apenas `required` (sem trocar kind) deve preservar asset_id e match_status
    services
        .project_service
        .update_variable_options(&proj.id, &v_photo.id, VariableKind::LinkedFile, false, None)
        .unwrap();
    let snap_same_kind = services.project_service.open_project(&proj.id).unwrap();
    let same_val = snap_same_kind.value_for("rec-1", &v_photo.id).unwrap();
    assert_eq!(same_val.asset_id.as_deref(), Some("asset-1"));
    assert_eq!(same_val.match_status, MatchStatus::AutoExact);
    assert_eq!(snap_same_kind.match_candidates.len(), 2);

    // Trocar kind de LinkedFile -> Text deve limpar asset_id, resetar match_status/match_confidence e remover candidatos da variável
    let updated = services
        .project_service
        .update_variable_options(
            &proj.id,
            &v_photo.id,
            VariableKind::Text,
            true,
            Some("NomeTpl".into()),
        )
        .unwrap();
    assert_eq!(updated.kind, VariableKind::Text);
    assert!(updated.required);
    assert_eq!(updated.mapped_template_var.as_deref(), Some("NomeTpl"));

    let snap_switched = services.project_service.open_project(&proj.id).unwrap();
    let cleared_val = snap_switched.value_for("rec-1", &v_photo.id).unwrap();
    assert_eq!(cleared_val.text_value, "00421");
    assert_eq!(cleared_val.asset_id, None);
    assert_eq!(cleared_val.match_status, MatchStatus::Unmatched);
    assert_eq!(cleared_val.match_confidence, None);

    // Outra variável não afetada deve manter vínculo e candidato intactos
    let untouched_val = snap_switched.value_for("rec-1", &v_other.id).unwrap();
    assert_eq!(untouched_val.asset_id.as_deref(), Some("asset-1"));
    assert_eq!(untouched_val.match_status, MatchStatus::Manual);
    assert_eq!(snap_switched.match_candidates.len(), 1);
    assert_eq!(snap_switched.match_candidates[0].variable_id, v_other.id);

    // Trocar de volta Text -> LinkedFile também executa limpeza sem erro
    services
        .project_service
        .update_variable_options(&proj.id, &v_photo.id, VariableKind::LinkedFile, true, None)
        .unwrap();
    let snap_back = services.project_service.open_project(&proj.id).unwrap();
    assert_eq!(snap_back.variable_by_id(&v_photo.id).unwrap().kind, VariableKind::LinkedFile);
}

fn read_table_columns(db_path: &std::path::Path, table_name: &str) -> Vec<String> {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table_name});"))
        .unwrap();
    stmt.query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

#[test]
fn cross_project_isolation_rejects_foreign_mutations_and_empty_delete_checks_project() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("aivm.db");
    let services = AppServices::new_with_db_path(&db_path).unwrap();

    let proj_a = services
        .project_service
        .create_project("Projeto A", ProjectMode::WithoutTemplate)
        .unwrap();
    let proj_b = services
        .project_service
        .create_project("Projeto B", ProjectMode::WithoutTemplate)
        .unwrap();

    let var_a = services
        .project_service
        .create_variable(&proj_a.id, "FotoA", Some("FotoA"), VariableKind::LinkedFile, true)
        .unwrap();
    let var_b = services
        .project_service
        .create_variable(&proj_b.id, "FotoB", Some("FotoB"), VariableKind::LinkedFile, true)
        .unwrap();

    let asset_a = Asset {
        id: "asset-a".into(),
        project_id: proj_a.id.clone(),
        source_path: PathBuf::from(r"C:\fotos\a.jpg"),
        file_name: "a.jpg".into(),
        stem: "a".into(),
        normalized_stem: "a".into(),
        format: ImageFormat::Jpg,
        file_size_bytes: 100,
        modified_epoch_ms: 1_700_000_000_000,
        exists_on_disk: true,
    };
    let asset_b = Asset {
        id: "asset-b".into(),
        project_id: proj_b.id.clone(),
        source_path: PathBuf::from(r"C:\fotos\b.jpg"),
        file_name: "b.jpg".into(),
        stem: "b".into(),
        normalized_stem: "b".into(),
        format: ImageFormat::Jpg,
        file_size_bytes: 200,
        modified_epoch_ms: 1_700_000_000_001,
        exists_on_disk: true,
    };
    services
        .project_repo
        .replace_assets(&proj_a.id, std::slice::from_ref(&asset_a))
        .unwrap();
    services
        .project_repo
        .replace_assets(&proj_b.id, std::slice::from_ref(&asset_b))
        .unwrap();

    let rec_a = Record {
        id: "rec-a".into(),
        project_id: proj_a.id.clone(),
        dataset_name: "DS_A".into(),
        sort_order: 0,
        created_at: "2026-11-17T00:00:00.000Z".into(),
        updated_at: "2026-11-17T00:00:00.000Z".into(),
    };
    let val_a = RecordValue {
        record_id: "rec-a".into(),
        variable_id: var_a.id.clone(),
        text_value: "ORIGINAL_A".into(),
        asset_id: Some("asset-a".into()),
        match_status: MatchStatus::Manual,
        match_confidence: Some(1.0),
    };
    services
        .project_repo
        .upsert_records_and_values(&proj_a.id, &[rec_a], &[val_a])
        .unwrap();

    let rec_b = Record {
        id: "rec-b".into(),
        project_id: proj_b.id.clone(),
        dataset_name: "DS_B".into(),
        sort_order: 0,
        created_at: "2026-11-17T00:00:00.000Z".into(),
        updated_at: "2026-11-17T00:00:00.000Z".into(),
    };
    services
        .project_repo
        .upsert_records_and_values(&proj_b.id, &[rec_b], &[])
        .unwrap();

    // 1. Tentativa de sobrescrever Record do Projeto A via ON CONFLICT(id) usando Projeto B
    let hijack_rec = Record {
        id: "rec-a".into(),
        project_id: proj_b.id.clone(),
        dataset_name: "HIJACKED".into(),
        sort_order: 5,
        created_at: "2026-11-17T00:00:00.000Z".into(),
        updated_at: "2026-11-17T00:00:00.000Z".into(),
    };
    let err = services
        .project_repo
        .upsert_records_and_values(&proj_b.id, &[hijack_rec], &[])
        .unwrap_err();
    assert!(matches!(err, AppError::Domain(DomainError::Conflict(_))));

    // 2. Tentativa de mutar RecordValue de A a partir de B
    let err = services
        .project_repo
        .upsert_records_and_values(
            &proj_b.id,
            &[],
            &[RecordValue::new_text("rec-a", &var_a.id, "PWNED")],
        )
        .unwrap_err();
    assert!(matches!(err, AppError::Domain(DomainError::Conflict(_))));

    // 3. Tentativa de criar célula EAV cruzada (rec_b, var_a) dentro de B
    let err = services
        .project_repo
        .upsert_records_and_values(
            &proj_b.id,
            &[],
            &[RecordValue::new_text("rec-b", &var_a.id, "CROSS_VAR")],
        )
        .unwrap_err();
    assert!(matches!(err, AppError::Domain(DomainError::Conflict(_))));

    // 4. Tentativa de vincular asset_a em RecordValue de B (com rollback de um registro novo no lote)
    let rec_b_new = Record {
        id: "rec-b-new".into(),
        project_id: proj_b.id.clone(),
        dataset_name: "DS_B_NEW".into(),
        sort_order: 1,
        created_at: "2026-11-17T00:00:00.000Z".into(),
        updated_at: "2026-11-17T00:00:00.000Z".into(),
    };
    let cross_asset_val = RecordValue {
        record_id: "rec-b-new".into(),
        variable_id: var_b.id.clone(),
        text_value: "a".into(),
        asset_id: Some("asset-a".into()),
        match_status: MatchStatus::Manual,
        match_confidence: Some(1.0),
    };
    let err = services
        .project_repo
        .upsert_records_and_values(&proj_b.id, &[rec_b_new], &[cross_asset_val])
        .unwrap_err();
    assert!(matches!(err, AppError::Domain(DomainError::Conflict(_))));

    // 5. Tentativa de replace_assets em B usando asset_id de A (deve falhar e preservar asset_b)
    let hijack_asset = Asset {
        id: "asset-a".into(),
        project_id: proj_b.id.clone(),
        source_path: PathBuf::from(r"C:\fotos\hijacked.jpg"),
        file_name: "hijacked.jpg".into(),
        stem: "hijacked".into(),
        normalized_stem: "hijacked".into(),
        format: ImageFormat::Jpg,
        file_size_bytes: 999,
        modified_epoch_ms: 1_700_000_000_999,
        exists_on_disk: true,
    };
    let err = services
        .project_repo
        .replace_assets(&proj_b.id, &[hijack_asset])
        .unwrap_err();
    assert!(matches!(err, AppError::Domain(DomainError::Conflict(_))));

    // 6. Tentativa de update_match_candidates em B referenciando registro/variável/ativo de A
    let valid_cand_b = MatchCandidate {
        record_id: "rec-b".into(),
        variable_id: var_b.id.clone(),
        asset_id: "asset-b".into(),
        score: 0.9,
        reason: "ok".into(),
    };
    services
        .project_repo
        .update_match_candidates(&proj_b.id, &[valid_cand_b])
        .unwrap();

    for bad_cand in [
        MatchCandidate {
            record_id: "rec-a".into(),
            variable_id: var_b.id.clone(),
            asset_id: "asset-b".into(),
            score: 1.0,
            reason: "foreign_rec".into(),
        },
        MatchCandidate {
            record_id: "rec-b".into(),
            variable_id: var_a.id.clone(),
            asset_id: "asset-b".into(),
            score: 1.0,
            reason: "foreign_var".into(),
        },
        MatchCandidate {
            record_id: "rec-b".into(),
            variable_id: var_b.id.clone(),
            asset_id: "asset-a".into(),
            score: 1.0,
            reason: "foreign_asset".into(),
        },
    ] {
        let err = services
            .project_repo
            .update_match_candidates(&proj_b.id, &[bad_cand])
            .unwrap_err();
        assert!(matches!(err, AppError::Domain(DomainError::Conflict(_))));
    }

    // 7. delete_records com slice vazia deve validar existência do projeto
    let err_missing_proj = services
        .project_repo
        .delete_records("projeto-inexistente", &[])
        .unwrap_err();
    assert!(matches!(
        err_missing_proj,
        AppError::Domain(DomainError::NotFound { .. })
    ));
    services.project_repo.delete_records(&proj_a.id, &[]).unwrap();

    // Verificar integridade total de Projeto A e rollback limpo em Projeto B
    let snap_a = services.project_service.open_project(&proj_a.id).unwrap();
    assert_eq!(snap_a.records.len(), 1);
    assert_eq!(snap_a.records[0].dataset_name, "DS_A");
    assert_eq!(snap_a.value_for("rec-a", &var_a.id).unwrap().text_value, "ORIGINAL_A");
    assert_eq!(snap_a.assets.len(), 1);
    assert_eq!(snap_a.assets[0].file_name, "a.jpg");
    assert!(snap_a.match_candidates.is_empty());

    let snap_b = services.project_service.open_project(&proj_b.id).unwrap();
    assert_eq!(snap_b.records.len(), 1);
    assert_eq!(snap_b.records[0].id, "rec-b");
    assert!(snap_b.values.is_empty());
    assert_eq!(snap_b.assets.len(), 1);
    assert_eq!(snap_b.assets[0].id, "asset-b");
    assert_eq!(snap_b.match_candidates.len(), 1);
}

#[test]
fn concurrent_new_with_db_path_and_create_variable_succeed() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("concurrent.db");

    // Fase 1: 12 threads abrindo AppServices simultaneamente em arquivo SQLite recém-criado
    let open_threads = 12usize;
    let open_barrier = Arc::new(Barrier::new(open_threads));
    let mut open_handles = Vec::with_capacity(open_threads);

    for i in 0..open_threads {
        let path = db_path.clone();
        let barrier = Arc::clone(&open_barrier);
        open_handles.push(std::thread::spawn(move || {
            barrier.wait();
            let svc = AppServices::new_with_db_path(&path)?;
            svc.project_service
                .create_project(&format!("Proj_{i}"), ProjectMode::WithoutTemplate)
        }));
    }

    let mut created_projects = Vec::new();
    for handle in open_handles {
        let proj = handle.join().unwrap().expect("Abertura/criação concorrente não deve falhar");
        created_projects.push(proj);
    }
    assert_eq!(created_projects.len(), open_threads);

    // Fase 2: 8 threads criando variáveis concorrentemente no mesmo projeto (xml_name distintos)
    let target_proj_id = created_projects[0].id.clone();
    let var_threads = 8usize;
    let var_barrier = Arc::new(Barrier::new(var_threads));
    let mut var_handles = Vec::with_capacity(var_threads);

    for i in 0..var_threads {
        let path = db_path.clone();
        let proj_id = target_proj_id.clone();
        let barrier = Arc::clone(&var_barrier);
        var_handles.push(std::thread::spawn(move || {
            let svc = AppServices::new_with_db_path(&path)?;
            barrier.wait();
            svc.project_service.create_variable(
                &proj_id,
                &format!("Campo {i}"),
                Some(&format!("Campo_{i}")),
                VariableKind::Text,
                false,
            )
        }));
    }

    for handle in var_handles {
        handle
            .join()
            .unwrap()
            .expect("create_variable concorrente com xml_name distinto deve serializar sem colisão de sort_order");
    }

    // Fase 3: 8 threads criando variáveis concorrentemente com xml_name = None e mesmo display_name
    let auto_barrier = Arc::new(Barrier::new(var_threads));
    let mut auto_handles = Vec::with_capacity(var_threads);
    for _ in 0..var_threads {
        let path = db_path.clone();
        let proj_id = target_proj_id.clone();
        let barrier = Arc::clone(&auto_barrier);
        auto_handles.push(std::thread::spawn(move || {
            let svc = AppServices::new_with_db_path(&path)?;
            barrier.wait();
            svc.project_service
                .create_variable(&proj_id, "Auto", None, VariableKind::Text, false)
        }));
    }

    for handle in auto_handles {
        handle
            .join()
            .unwrap()
            .expect("create_variable concorrente com xml_name = None deve resolver unicidade sem erro");
    }

    let svc = AppServices::new_with_db_path(&db_path).unwrap();
    let snap = svc.project_service.open_project(&target_proj_id).unwrap();
    assert_eq!(snap.variables.len(), var_threads * 2);
    let orders: Vec<usize> = snap.variables.iter().map(|v| v.sort_order).collect();
    assert_eq!(orders, (0..var_threads * 2).collect::<Vec<_>>());
    let xml_names: HashSet<&str> = snap.variables.iter().map(|v| v.xml_name.as_str()).collect();
    assert_eq!(xml_names.len(), var_threads * 2);

    // Conflito explícito de xml_name continua falhando com Conflict
    let err_dup = svc
        .project_service
        .create_variable(&target_proj_id, "Dup", Some("Campo_0"), VariableKind::Text, false)
        .unwrap_err();
    assert!(matches!(err_dup, AppError::Domain(DomainError::Conflict(_))));
}

#[test]
fn reindex_handles_i64_max_sort_order_without_panic() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("extreme_order.db");
    let services = AppServices::new_with_db_path(&db_path).unwrap();

    let proj = services
        .project_service
        .create_project("Ordem Extrema", ProjectMode::WithoutTemplate)
        .unwrap();

    let max_order = i64::MAX as usize;
    let v0 = Variable {
        id: "v-zero".into(),
        project_id: proj.id.clone(),
        display_name: "Zero".into(),
        xml_name: "Zero".into(),
        kind: VariableKind::Text,
        required: false,
        sort_order: 0,
        mapped_template_var: None,
    };
    let vmax = Variable {
        id: "v-max".into(),
        project_id: proj.id.clone(),
        display_name: "Maximo".into(),
        xml_name: "Maximo".into(),
        kind: VariableKind::Text,
        required: false,
        sort_order: max_order,
        mapped_template_var: None,
    };
    services.project_repo.upsert_variable(&v0).unwrap();
    services.project_repo.upsert_variable(&vmax).unwrap();

    // Duplicar v-zero exigiria deslocar v-max (i64::MAX + 1) -> deve retornar Validation sem panic
    let dup_err = services
        .project_repo
        .duplicate_variable(&proj.id, "v-zero")
        .unwrap_err();
    assert!(matches!(
        dup_err,
        AppError::Domain(DomainError::Validation(_))
    ));

    // Reordenar variáveis com sort_order = i64::MAX deve funcionar sem panic
    services
        .project_repo
        .reorder_variables(&proj.id, &["v-max".into(), "v-zero".into()])
        .unwrap();
    let snap_vars = services.project_service.open_project(&proj.id).unwrap();
    let var_orders: Vec<(&str, usize)> = snap_vars
        .variables
        .iter()
        .map(|v| (v.id.as_str(), v.sort_order))
        .collect();
    assert_eq!(var_orders, vec![("v-max", 0), ("v-zero", 1)]);

    // Reordenar registros com sort_order = i64::MAX deve funcionar sem panic
    let r0 = Record {
        id: "r-zero".into(),
        project_id: proj.id.clone(),
        dataset_name: "DS_0".into(),
        sort_order: 0,
        created_at: "2026-11-17T00:00:00.000Z".into(),
        updated_at: "2026-11-17T00:00:00.000Z".into(),
    };
    let rmax = Record {
        id: "r-max".into(),
        project_id: proj.id.clone(),
        dataset_name: "DS_MAX".into(),
        sort_order: max_order,
        created_at: "2026-11-17T00:00:00.000Z".into(),
        updated_at: "2026-11-17T00:00:00.000Z".into(),
    };
    services
        .project_repo
        .upsert_records_and_values(&proj.id, &[r0, rmax], &[])
        .unwrap();

    services
        .project_repo
        .reorder_records(&proj.id, &["r-max".into(), "r-zero".into()])
        .unwrap();
    let snap_recs = services.project_service.open_project(&proj.id).unwrap();
    let rec_orders: Vec<(&str, usize)> = snap_recs
        .records
        .iter()
        .map(|r| (r.id.as_str(), r.sort_order))
        .collect();
    assert_eq!(rec_orders, vec![("r-max", 0), ("r-zero", 1)]);
}

fn assert_clean_user_message(err: &AppError) {
    let msg = err.user_message();
    assert!(
        !msg.trim().is_empty(),
        "user_message() não deve ser vazio"
    );
    for forbidden in [
        "UNIQUE constraint",
        "CHECK constraint",
        "FOREIGN KEY",
        "NOT NULL constraint",
        "variables.",
        "records.",
        "assets.",
        "SQLITE_",
        "sort_order",
        "xml_name",
    ] {
        assert!(
            !msg.contains(forbidden),
            "user_message() vazou '{forbidden}': {msg}"
        );
    }
}

#[test]
fn constraint_errors_never_leak_sqlite_internals_in_user_message() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("no_leak.db");
    let services = AppServices::new_with_db_path(&db_path).unwrap();

    let proj = services
        .project_service
        .create_project("Mensagens Limpas", ProjectMode::WithoutTemplate)
        .unwrap();

    services
        .project_service
        .create_variable(&proj.id, "Nome", Some("Nome"), VariableKind::Text, true)
        .unwrap();

    // 1. upsert_variable com xml_name duplicado
    let dup_xml = Variable {
        id: "v-dup-xml".into(),
        project_id: proj.id.clone(),
        display_name: "Outro".into(),
        xml_name: "Nome".into(),
        kind: VariableKind::Text,
        required: false,
        sort_order: 1,
        mapped_template_var: None,
    };
    let err_xml = services.project_repo.upsert_variable(&dup_xml).unwrap_err();
    assert!(matches!(err_xml, AppError::Domain(DomainError::Conflict(_))));
    assert_clean_user_message(&err_xml);

    // 2. upsert_variable com sort_order duplicado
    let dup_order = Variable {
        id: "v-dup-order".into(),
        project_id: proj.id.clone(),
        display_name: "Cargo".into(),
        xml_name: "Cargo".into(),
        kind: VariableKind::Text,
        required: false,
        sort_order: 0,
        mapped_template_var: None,
    };
    let err_var_order = services
        .project_repo
        .upsert_variable(&dup_order)
        .unwrap_err();
    assert!(matches!(
        err_var_order,
        AppError::Domain(DomainError::Conflict(_))
    ));
    assert_clean_user_message(&err_var_order);

    // 3. upsert_records_and_values com sort_order duplicado
    let r1 = Record {
        id: "rec-1".into(),
        project_id: proj.id.clone(),
        dataset_name: "DS_1".into(),
        sort_order: 0,
        created_at: "2026-11-17T00:00:00.000Z".into(),
        updated_at: "2026-11-17T00:00:00.000Z".into(),
    };
    let r2 = Record {
        id: "rec-2".into(),
        project_id: proj.id.clone(),
        dataset_name: "DS_2".into(),
        sort_order: 0,
        created_at: "2026-11-17T00:00:00.000Z".into(),
        updated_at: "2026-11-17T00:00:00.000Z".into(),
    };
    let err_rec_order = services
        .project_repo
        .upsert_records_and_values(&proj.id, &[r1, r2], &[])
        .unwrap_err();
    assert!(matches!(
        err_rec_order,
        AppError::Domain(DomainError::Conflict(_))
    ));
    assert_clean_user_message(&err_rec_order);

    // 4. replace_assets com source_path duplicado no mesmo projeto
    let asset_a = Asset {
        id: "asset-a".into(),
        project_id: proj.id.clone(),
        source_path: PathBuf::from(r"C:\fotos\duplicada.jpg"),
        file_name: "duplicada.jpg".into(),
        stem: "duplicada".into(),
        normalized_stem: "duplicada".into(),
        format: ImageFormat::Jpg,
        file_size_bytes: 123,
        modified_epoch_ms: 1_700_000_000_000,
        exists_on_disk: true,
    };
    let asset_b = Asset {
        id: "asset-b".into(),
        project_id: proj.id.clone(),
        source_path: PathBuf::from(r"C:\fotos\duplicada.jpg"),
        file_name: "duplicada.jpg".into(),
        stem: "duplicada".into(),
        normalized_stem: "duplicada".into(),
        format: ImageFormat::Jpg,
        file_size_bytes: 456,
        modified_epoch_ms: 1_700_000_000_001,
        exists_on_disk: true,
    };
    let err_asset_path = services
        .project_repo
        .replace_assets(&proj.id, &[asset_a, asset_b])
        .unwrap_err();
    assert!(matches!(
        err_asset_path,
        AppError::Domain(DomainError::Conflict(_))
    ));
    assert_clean_user_message(&err_asset_path);

    // 5. duplicate_variable em i64::MAX (tanto o próprio quanto o adjacente)
    let vmax = Variable {
        id: "v-max".into(),
        project_id: proj.id.clone(),
        display_name: "Maximo".into(),
        xml_name: "Maximo".into(),
        kind: VariableKind::Text,
        required: false,
        sort_order: i64::MAX as usize,
        mapped_template_var: None,
    };
    let vprev = Variable {
        id: "v-prev".into(),
        project_id: proj.id.clone(),
        display_name: "Anterior".into(),
        xml_name: "Anterior".into(),
        kind: VariableKind::Text,
        required: false,
        sort_order: (i64::MAX as usize) - 1,
        mapped_template_var: None,
    };
    services.project_repo.upsert_variable(&vmax).unwrap();
    services.project_repo.upsert_variable(&vprev).unwrap();

    let err_dup_max = services
        .project_repo
        .duplicate_variable(&proj.id, "v-max")
        .unwrap_err();
    assert!(matches!(
        err_dup_max,
        AppError::Domain(DomainError::Validation(_))
    ));
    assert_clean_user_message(&err_dup_max);

    let err_dup_prev = services
        .project_repo
        .duplicate_variable(&proj.id, "v-prev")
        .unwrap_err();
    assert!(matches!(
        err_dup_prev,
        AppError::Domain(DomainError::Validation(_))
    ));
    assert_clean_user_message(&err_dup_prev);
}

struct ExplicitXmlRaceRepo {
    inner: Arc<dyn ProjectRepository>,
    upsert_calls: AtomicUsize,
}

impl ProjectRepository for ExplicitXmlRaceRepo {
    fn create_project(&self, name: &str, mode: ProjectMode) -> AppResult<Project> {
        self.inner.create_project(name, mode)
    }
    fn open_project(&self, project_id: &str) -> AppResult<ProjectSnapshot> {
        self.inner.open_project(project_id)
    }
    fn list_projects(&self) -> AppResult<Vec<ProjectSummary>> {
        self.inner.list_projects()
    }
    fn rename_project(&self, project_id: &str, new_name: &str) -> AppResult<()> {
        self.inner.rename_project(project_id, new_name)
    }
    fn delete_project(&self, project_id: &str) -> AppResult<()> {
        self.inner.delete_project(project_id)
    }
    fn update_project_settings(&self, project: &Project) -> AppResult<()> {
        self.inner.update_project_settings(project)
    }
    fn upsert_variable(&self, var: &Variable) -> AppResult<()> {
        let call_idx = self.upsert_calls.fetch_add(1, Ordering::SeqCst);
        if call_idx == 0 {
            let racer = Variable {
                id: "racer-xml-only".into(),
                project_id: var.project_id.clone(),
                display_name: "Competidora".into(),
                xml_name: var.xml_name.clone(),
                kind: VariableKind::Text,
                required: false,
                sort_order: var.sort_order + 50,
                mapped_template_var: None,
            };
            self.inner.upsert_variable(&racer)?;
        }
        self.inner.upsert_variable(var)
    }
    fn duplicate_variable(&self, project_id: &str, variable_id: &str) -> AppResult<Variable> {
        self.inner.duplicate_variable(project_id, variable_id)
    }
    fn delete_variable(&self, project_id: &str, variable_id: &str) -> AppResult<()> {
        self.inner.delete_variable(project_id, variable_id)
    }
    fn reorder_variables(&self, project_id: &str, ordered_ids: &[String]) -> AppResult<()> {
        self.inner.reorder_variables(project_id, ordered_ids)
    }
    fn upsert_records_and_values(
        &self,
        project_id: &str,
        records: &[Record],
        values: &[RecordValue],
    ) -> AppResult<()> {
        self.inner
            .upsert_records_and_values(project_id, records, values)
    }
    fn reorder_records(&self, project_id: &str, ordered_ids: &[String]) -> AppResult<()> {
        self.inner.reorder_records(project_id, ordered_ids)
    }
    fn delete_records(&self, project_id: &str, record_ids: &[String]) -> AppResult<()> {
        self.inner.delete_records(project_id, record_ids)
    }
    fn replace_assets(&self, project_id: &str, assets: &[Asset]) -> AppResult<()> {
        self.inner.replace_assets(project_id, assets)
    }
    fn update_match_candidates(
        &self,
        project_id: &str,
        candidates: &[MatchCandidate],
    ) -> AppResult<()> {
        self.inner.update_match_candidates(project_id, candidates)
    }
    fn get_setting(&self, key: &str) -> AppResult<Option<String>> {
        self.inner.get_setting(key)
    }
    fn set_setting(&self, key: &str, value: &str) -> AppResult<()> {
        self.inner.set_setting(key, value)
    }
}

#[test]
fn create_variable_explicit_xml_toctou_race_fails_immediately_with_clean_message() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("toctou_xml.db");
    let services = AppServices::new_with_db_path(&db_path).unwrap();
    let proj = services
        .project_service
        .create_project("TOCTOU XML", ProjectMode::WithoutTemplate)
        .unwrap();

    let race_repo = Arc::new(ExplicitXmlRaceRepo {
        inner: Arc::clone(&services.project_repo),
        upsert_calls: AtomicUsize::new(0),
    });
    let svc = ProjectService::new(race_repo.clone());

    let err = svc
        .create_variable(&proj.id, "Alvo", Some("Alvo"), VariableKind::Text, false)
        .unwrap_err();
    assert!(matches!(err, AppError::Domain(DomainError::Conflict(_))));
    assert_clean_user_message(&err);
    assert_eq!(
        race_repo.upsert_calls.load(Ordering::SeqCst),
        1,
        "Colisão em xml_name explícito deve falhar imediatamente sem novas tentativas"
    );
}

struct RoundState {
    expected: usize,
    open_arrived: usize,
    open_gen: u64,
    done_arrived: usize,
    done_wins: usize,
    done_gen: u64,
}

struct RoundBarrierRepo {
    inner: Arc<dyn ProjectRepository>,
    state: Mutex<RoundState>,
    cvar: Condvar,
}

impl RoundBarrierRepo {
    fn new(inner: Arc<dyn ProjectRepository>, initial_threads: usize) -> Self {
        Self {
            inner,
            state: Mutex::new(RoundState {
                expected: initial_threads,
                open_arrived: 0,
                open_gen: 0,
                done_arrived: 0,
                done_wins: 0,
                done_gen: 0,
            }),
            cvar: Condvar::new(),
        }
    }
}

impl ProjectRepository for RoundBarrierRepo {
    fn create_project(&self, name: &str, mode: ProjectMode) -> AppResult<Project> {
        self.inner.create_project(name, mode)
    }
    fn open_project(&self, project_id: &str) -> AppResult<ProjectSnapshot> {
        {
            let mut st = self.state.lock().unwrap();
            let my_gen = st.open_gen;
            st.open_arrived += 1;
            if st.open_arrived >= st.expected {
                st.open_arrived = 0;
                st.open_gen += 1;
                self.cvar.notify_all();
            } else {
                while st.open_gen == my_gen {
                    st = self.cvar.wait(st).unwrap();
                }
            }
        }
        self.inner.open_project(project_id)
    }
    fn list_projects(&self) -> AppResult<Vec<ProjectSummary>> {
        self.inner.list_projects()
    }
    fn rename_project(&self, project_id: &str, new_name: &str) -> AppResult<()> {
        self.inner.rename_project(project_id, new_name)
    }
    fn delete_project(&self, project_id: &str) -> AppResult<()> {
        self.inner.delete_project(project_id)
    }
    fn update_project_settings(&self, project: &Project) -> AppResult<()> {
        self.inner.update_project_settings(project)
    }
    fn upsert_variable(&self, var: &Variable) -> AppResult<()> {
        let res = self.inner.upsert_variable(var);
        let won_or_terminal = res.is_ok();
        {
            let mut st = self.state.lock().unwrap();
            let my_gen = st.done_gen;
            st.done_arrived += 1;
            if won_or_terminal {
                st.done_wins += 1;
            }
            if st.done_arrived >= st.expected {
                st.expected -= st.done_wins;
                st.done_arrived = 0;
                st.done_wins = 0;
                st.done_gen += 1;
                self.cvar.notify_all();
            } else {
                while st.done_gen == my_gen {
                    st = self.cvar.wait(st).unwrap();
                }
            }
        }
        res
    }
    fn duplicate_variable(&self, project_id: &str, variable_id: &str) -> AppResult<Variable> {
        self.inner.duplicate_variable(project_id, variable_id)
    }
    fn delete_variable(&self, project_id: &str, variable_id: &str) -> AppResult<()> {
        self.inner.delete_variable(project_id, variable_id)
    }
    fn reorder_variables(&self, project_id: &str, ordered_ids: &[String]) -> AppResult<()> {
        self.inner.reorder_variables(project_id, ordered_ids)
    }
    fn upsert_records_and_values(
        &self,
        project_id: &str,
        records: &[Record],
        values: &[RecordValue],
    ) -> AppResult<()> {
        self.inner
            .upsert_records_and_values(project_id, records, values)
    }
    fn reorder_records(&self, project_id: &str, ordered_ids: &[String]) -> AppResult<()> {
        self.inner.reorder_records(project_id, ordered_ids)
    }
    fn delete_records(&self, project_id: &str, record_ids: &[String]) -> AppResult<()> {
        self.inner.delete_records(project_id, record_ids)
    }
    fn replace_assets(&self, project_id: &str, assets: &[Asset]) -> AppResult<()> {
        self.inner.replace_assets(project_id, assets)
    }
    fn update_match_candidates(
        &self,
        project_id: &str,
        candidates: &[MatchCandidate],
    ) -> AppResult<()> {
        self.inner.update_match_candidates(project_id, candidates)
    }
    fn get_setting(&self, key: &str) -> AppResult<Option<String>> {
        self.inner.get_setting(key)
    }
    fn set_setting(&self, key: &str, value: &str) -> AppResult<()> {
        self.inner.set_setting(key, value)
    }
}

struct AlwaysSortOrderConflictRepo {
    inner: Arc<dyn ProjectRepository>,
    upsert_calls: AtomicUsize,
}

impl ProjectRepository for AlwaysSortOrderConflictRepo {
    fn create_project(&self, name: &str, mode: ProjectMode) -> AppResult<Project> {
        self.inner.create_project(name, mode)
    }
    fn open_project(&self, project_id: &str) -> AppResult<ProjectSnapshot> {
        self.inner.open_project(project_id)
    }
    fn list_projects(&self) -> AppResult<Vec<ProjectSummary>> {
        self.inner.list_projects()
    }
    fn rename_project(&self, project_id: &str, new_name: &str) -> AppResult<()> {
        self.inner.rename_project(project_id, new_name)
    }
    fn delete_project(&self, project_id: &str) -> AppResult<()> {
        self.inner.delete_project(project_id)
    }
    fn update_project_settings(&self, project: &Project) -> AppResult<()> {
        self.inner.update_project_settings(project)
    }
    fn upsert_variable(&self, var: &Variable) -> AppResult<()> {
        let idx = self.upsert_calls.fetch_add(1, Ordering::SeqCst);
        let blocker = Variable {
            id: format!("blocker-{idx}"),
            project_id: var.project_id.clone(),
            display_name: format!("Bloqueio {idx}"),
            xml_name: format!("Bloqueio_{idx}"),
            kind: VariableKind::Text,
            required: false,
            sort_order: var.sort_order,
            mapped_template_var: None,
        };
        self.inner.upsert_variable(&blocker)?;
        self.inner.upsert_variable(var)
    }
    fn duplicate_variable(&self, project_id: &str, variable_id: &str) -> AppResult<Variable> {
        self.inner.duplicate_variable(project_id, variable_id)
    }
    fn delete_variable(&self, project_id: &str, variable_id: &str) -> AppResult<()> {
        self.inner.delete_variable(project_id, variable_id)
    }
    fn reorder_variables(&self, project_id: &str, ordered_ids: &[String]) -> AppResult<()> {
        self.inner.reorder_variables(project_id, ordered_ids)
    }
    fn upsert_records_and_values(
        &self,
        project_id: &str,
        records: &[Record],
        values: &[RecordValue],
    ) -> AppResult<()> {
        self.inner
            .upsert_records_and_values(project_id, records, values)
    }
    fn reorder_records(&self, project_id: &str, ordered_ids: &[String]) -> AppResult<()> {
        self.inner.reorder_records(project_id, ordered_ids)
    }
    fn delete_records(&self, project_id: &str, record_ids: &[String]) -> AppResult<()> {
        self.inner.delete_records(project_id, record_ids)
    }
    fn replace_assets(&self, project_id: &str, assets: &[Asset]) -> AppResult<()> {
        self.inner.replace_assets(project_id, assets)
    }
    fn update_match_candidates(
        &self,
        project_id: &str,
        candidates: &[MatchCandidate],
    ) -> AppResult<()> {
        self.inner.update_match_candidates(project_id, candidates)
    }
    fn get_setting(&self, key: &str) -> AppResult<Option<String>> {
        self.inner.get_setting(key)
    }
    fn set_setting(&self, key: &str, value: &str) -> AppResult<()> {
        self.inner.set_setting(key, value)
    }
}

#[test]
fn create_variable_lockstep_40_threads_succeed_and_retry_exhaustion_returns_clean_conflict() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("lockstep_and_exhaust.db");
    let services = AppServices::new_with_db_path(&db_path).unwrap();

    // Parte A: 40 threads sincronizadas em lockstep (cada rodada lê o mesmo snapshot) devem todas concluir com sucesso
    let proj_lockstep = services
        .project_service
        .create_project("Lockstep 40", ProjectMode::WithoutTemplate)
        .unwrap();
    let thread_count = 40usize;
    let barrier_repo = Arc::new(RoundBarrierRepo::new(
        Arc::clone(&services.project_repo),
        thread_count,
    ));
    let lockstep_svc = Arc::new(ProjectService::new(barrier_repo));

    let mut handles = Vec::with_capacity(thread_count);
    for _ in 0..thread_count {
        let svc = Arc::clone(&lockstep_svc);
        let pid = proj_lockstep.id.clone();
        handles.push(std::thread::spawn(move || {
            svc.create_variable(&pid, "Campo", None, VariableKind::Text, false)
        }));
    }

    for h in handles {
        h.join()
            .unwrap()
            .expect("40 leitores em lockstep devem concluir sem esgotar tentativas");
    }

    let snap_lockstep = services.project_service.open_project(&proj_lockstep.id).unwrap();
    assert_eq!(snap_lockstep.variables.len(), thread_count);
    let orders: Vec<usize> = snap_lockstep.variables.iter().map(|v| v.sort_order).collect();
    assert_eq!(orders, (0..thread_count).collect::<Vec<_>>());

    // Parte B: esgotamento real de tentativas deve retornar a mensagem pt-BR limpa de conflito (sem vazar SQLite)
    let proj_exhaust = services
        .project_service
        .create_project("Exaustao", ProjectMode::WithoutTemplate)
        .unwrap();
    let exhaust_repo = Arc::new(AlwaysSortOrderConflictRepo {
        inner: Arc::clone(&services.project_repo),
        upsert_calls: AtomicUsize::new(0),
    });
    let exhaust_svc = ProjectService::new(exhaust_repo.clone());

    let err = exhaust_svc
        .create_variable(&proj_exhaust.id, "Campo", None, VariableKind::Text, false)
        .unwrap_err();
    assert_eq!(exhaust_repo.upsert_calls.load(Ordering::SeqCst), 128);
    assert_clean_user_message(&err);
    match &err {
        AppError::Domain(DomainError::Conflict(msg)) => {
            assert_eq!(
                msg,
                "Não foi possível criar a variável após múltiplas tentativas concorrentes."
            );
        }
        other => panic!("Esperava DomainError::Conflict de esgotamento, obteve {other:?}"),
    }
}
