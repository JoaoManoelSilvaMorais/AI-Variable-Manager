use std::collections::HashSet;
use std::path::PathBuf;

use rusqlite::{OptionalExtension, Transaction, params};
use uuid::Uuid;

use crate::domain::asset::{Asset, ImageFormat, MatchCandidate};
use crate::domain::ports::ProjectRepository;
use crate::domain::project::{PathStrategy, Project, ProjectMode, ProjectSnapshot, ProjectSummary};
use crate::domain::record::{MatchStatus, Record, RecordValue, compute_reordered_record_ids};
use crate::domain::variable::{
    Variable, VariableKind, build_duplicated_variable, compute_reordered_ids,
};
use crate::infrastructure::sqlite::connection::{SqliteConnectionManager, map_sqlite_err};
use crate::shared::error::{AppError, AppResult};
use crate::shared::time::utc_now_rfc3339;

pub struct SqliteProjectRepository {
    conn_mgr: SqliteConnectionManager,
}

impl SqliteProjectRepository {
    pub fn new(conn_mgr: SqliteConnectionManager) -> Self {
        Self { conn_mgr }
    }

    pub fn conn_mgr(&self) -> &SqliteConnectionManager {
        &self.conn_mgr
    }
}

fn ensure_project_exists_tx(tx: &Transaction<'_>, project_id: &str) -> AppResult<()> {
    let exists: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM projects WHERE id = ?1;",
            params![project_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(map_sqlite_err)?;
    if exists.is_none() {
        return Err(AppError::not_found("Projeto", project_id));
    }
    Ok(())
}

fn touch_project_tx(tx: &Transaction<'_>, project_id: &str, now: &str) -> AppResult<()> {
    tx.execute(
        "UPDATE projects SET updated_at = ?2 WHERE id = ?1;",
        params![project_id, now],
    )
    .map_err(map_sqlite_err)?;
    Ok(())
}

fn to_usize(val: i64, field: &str) -> AppResult<usize> {
    usize::try_from(val).map_err(|_| {
        AppError::database(
            "conversão de inteiro",
            format!("Valor negativo inesperado para '{field}': {val}"),
        )
    })
}

fn to_u64(val: i64, field: &str) -> AppResult<u64> {
    u64::try_from(val).map_err(|_| {
        AppError::database(
            "conversão de inteiro",
            format!("Valor negativo inesperado para '{field}': {val}"),
        )
    })
}

fn to_i64(val: usize, field: &str) -> AppResult<i64> {
    i64::try_from(val).map_err(|_| {
        AppError::validation(format!("Valor excede limite suportado para '{field}': {val}"))
    })
}

fn ensure_entity_in_project_tx(
    tx: &Transaction<'_>,
    sql: &str,
    entity_label: &'static str,
    conflict_msg: &str,
    entity_id: &str,
    project_id: &str,
) -> AppResult<()> {
    let owner: Option<String> = {
        let mut stmt = tx.prepare_cached(sql).map_err(map_sqlite_err)?;
        stmt.query_row(params![entity_id], |r| r.get::<_, String>(0))
            .optional()
            .map_err(map_sqlite_err)?
    };
    match owner {
        None => Err(AppError::not_found(entity_label, entity_id)),
        Some(owner_proj) if owner_proj != project_id => {
            Err(AppError::conflict(format!("{conflict_msg}: '{entity_id}'")))
        }
        Some(_) => Ok(()),
    }
}

fn compute_temp_negative_order(min_order: i64, idx: usize) -> AppResult<i64> {
    let step = i64::try_from(idx)
        .ok()
        .and_then(|i| i.checked_add(1))
        .ok_or_else(|| AppError::validation("Quantidade de itens excede limite i64."))?;
    if min_order >= 0 {
        step.checked_neg().ok_or_else(|| {
            AppError::validation("Overflow ao calcular ordem temporária de reindexação.")
        })
    } else {
        min_order.checked_sub(step).ok_or_else(|| {
            AppError::validation("Overflow ao calcular ordem temporária de reindexação.")
        })
    }
}

fn reindex_variables_tx(
    tx: &Transaction<'_>,
    project_id: &str,
    ordered_ids: &[String],
) -> AppResult<()> {
    if ordered_ids.is_empty() {
        return Ok(());
    }
    let min_order: i64 = tx
        .query_row(
            "SELECT COALESCE(MIN(sort_order), 0) FROM variables WHERE project_id = ?1;",
            params![project_id],
            |r| r.get(0),
        )
        .map_err(map_sqlite_err)?;

    // Fase 1: mover todas as linhas para o espaço negativo disjunto para evitar colisão de UNIQUE(project_id, sort_order)
    {
        let mut stmt = tx
            .prepare_cached("UPDATE variables SET sort_order = ?1 WHERE id = ?2 AND project_id = ?3;")
            .map_err(map_sqlite_err)?;
        for (idx, id) in ordered_ids.iter().enumerate() {
            let temp_order = compute_temp_negative_order(min_order, idx)?;
            stmt.execute(params![temp_order, id, project_id])
                .map_err(map_sqlite_err)?;
        }
    }

    // Fase 2: atribuir a ordem contígua final 0..N-1
    {
        let mut stmt = tx
            .prepare_cached("UPDATE variables SET sort_order = ?1 WHERE id = ?2 AND project_id = ?3;")
            .map_err(map_sqlite_err)?;
        for (idx, id) in ordered_ids.iter().enumerate() {
            let final_order = to_i64(idx, "ordem da variável")?;
            stmt.execute(params![final_order, id, project_id])
                .map_err(map_sqlite_err)?;
        }
    }

    Ok(())
}

fn reindex_records_tx(
    tx: &Transaction<'_>,
    project_id: &str,
    ordered_ids: &[String],
) -> AppResult<()> {
    if ordered_ids.is_empty() {
        return Ok(());
    }
    let min_order: i64 = tx
        .query_row(
            "SELECT COALESCE(MIN(sort_order), 0) FROM records WHERE project_id = ?1;",
            params![project_id],
            |r| r.get(0),
        )
        .map_err(map_sqlite_err)?;

    {
        let mut stmt = tx
            .prepare_cached("UPDATE records SET sort_order = ?1 WHERE id = ?2 AND project_id = ?3;")
            .map_err(map_sqlite_err)?;
        for (idx, id) in ordered_ids.iter().enumerate() {
            let temp_order = compute_temp_negative_order(min_order, idx)?;
            stmt.execute(params![temp_order, id, project_id])
                .map_err(map_sqlite_err)?;
        }
    }

    {
        let mut stmt = tx
            .prepare_cached("UPDATE records SET sort_order = ?1 WHERE id = ?2 AND project_id = ?3;")
            .map_err(map_sqlite_err)?;
        for (idx, id) in ordered_ids.iter().enumerate() {
            let final_order = to_i64(idx, "ordem do registro")?;
            stmt.execute(params![final_order, id, project_id])
                .map_err(map_sqlite_err)?;
        }
    }

    Ok(())
}

fn opt_path_to_str(path: &Option<PathBuf>) -> Option<String> {
    path.as_ref().map(|p| p.to_string_lossy().into_owned())
}

fn load_variables_tx(tx: &Transaction<'_>, project_id: &str) -> AppResult<Vec<Variable>> {
    let mut stmt = tx
        .prepare(
            "SELECT id, project_id, display_name, xml_name, kind, required, sort_order, mapped_template_var
             FROM variables WHERE project_id = ?1 ORDER BY sort_order ASC;",
        )
        .map_err(map_sqlite_err)?;
    let rows = stmt
        .query_map(params![project_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, Option<String>>(7)?,
            ))
        })
        .map_err(map_sqlite_err)?;
    let mut out = Vec::new();
    for r in rows {
        let row = r.map_err(map_sqlite_err)?;
        out.push(Variable {
            id: row.0,
            project_id: row.1,
            display_name: row.2,
            xml_name: row.3,
            kind: VariableKind::from_db_str(&row.4)?,
            required: row.5 != 0,
            sort_order: to_usize(row.6, "variables.sort_order")?,
            mapped_template_var: row.7,
        });
    }
    Ok(out)
}

fn load_records_tx(tx: &Transaction<'_>, project_id: &str) -> AppResult<Vec<Record>> {
    let mut stmt = tx
        .prepare(
            "SELECT id, project_id, dataset_name, sort_order, created_at, updated_at
             FROM records WHERE project_id = ?1 ORDER BY sort_order ASC;",
        )
        .map_err(map_sqlite_err)?;
    let rows = stmt
        .query_map(params![project_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })
        .map_err(map_sqlite_err)?;
    let mut out = Vec::new();
    for r in rows {
        let row = r.map_err(map_sqlite_err)?;
        out.push(Record {
            id: row.0,
            project_id: row.1,
            dataset_name: row.2,
            sort_order: to_usize(row.3, "records.sort_order")?,
            created_at: row.4,
            updated_at: row.5,
        });
    }
    Ok(out)
}

impl ProjectRepository for SqliteProjectRepository {
    fn create_project(&self, name: &str, mode: ProjectMode) -> AppResult<Project> {
        let id = Uuid::new_v4().to_string();
        let now = utc_now_rfc3339();
        let path_strategy = PathStrategy::RelativeToXml;

        self.conn_mgr.with_immediate_tx(|tx| {
            tx.execute(
                "INSERT INTO projects (
                    id, name, mode, path_strategy,
                    xml_template_path, xml_template_raw, photo_root_dir, last_export_dir,
                    created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, NULL, NULL, ?5, ?5);",
                params![id, name, mode.as_db_str(), path_strategy.as_db_str(), now],
            )
            .map_err(map_sqlite_err)?;
            Ok(())
        })?;

        Ok(Project {
            id,
            name: name.to_string(),
            mode,
            path_strategy,
            xml_template_path: None,
            xml_template_raw: None,
            photo_root_dir: None,
            last_export_dir: None,
            created_at: now.clone(),
            updated_at: now,
        })
    }

    fn open_project(&self, project_id: &str) -> AppResult<ProjectSnapshot> {
        self.conn_mgr.with_read_conn(|conn| {
            let project_row = conn
                .query_row(
                    "SELECT id, name, mode, path_strategy, xml_template_path, xml_template_raw,
                            photo_root_dir, last_export_dir, created_at, updated_at
                     FROM projects WHERE id = ?1;",
                    params![project_id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, Option<String>>(4)?,
                            r.get::<_, Option<String>>(5)?,
                            r.get::<_, Option<String>>(6)?,
                            r.get::<_, Option<String>>(7)?,
                            r.get::<_, String>(8)?,
                            r.get::<_, String>(9)?,
                        ))
                    },
                )
                .optional()
                .map_err(map_sqlite_err)?
                .ok_or_else(|| AppError::not_found("Projeto", project_id))?;

            let project = Project {
                id: project_row.0,
                name: project_row.1,
                mode: ProjectMode::from_db_str(&project_row.2)?,
                path_strategy: PathStrategy::from_db_str(&project_row.3)?,
                xml_template_path: project_row.4.map(PathBuf::from),
                xml_template_raw: project_row.5,
                photo_root_dir: project_row.6.map(PathBuf::from),
                last_export_dir: project_row.7.map(PathBuf::from),
                created_at: project_row.8,
                updated_at: project_row.9,
            };

            let mut var_stmt = conn
                .prepare(
                    "SELECT id, project_id, display_name, xml_name, kind, required, sort_order, mapped_template_var
                     FROM variables WHERE project_id = ?1 ORDER BY sort_order ASC;",
                )
                .map_err(map_sqlite_err)?;
            let var_rows = var_stmt
                .query_map(params![project_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, i64>(6)?,
                        r.get::<_, Option<String>>(7)?,
                    ))
                })
                .map_err(map_sqlite_err)?;

            let mut variables = Vec::new();
            for row in var_rows {
                let row = row.map_err(map_sqlite_err)?;
                variables.push(Variable {
                    id: row.0,
                    project_id: row.1,
                    display_name: row.2,
                    xml_name: row.3,
                    kind: VariableKind::from_db_str(&row.4)?,
                    required: row.5 != 0,
                    sort_order: to_usize(row.6, "variables.sort_order")?,
                    mapped_template_var: row.7,
                });
            }

            let mut rec_stmt = conn
                .prepare(
                    "SELECT id, project_id, dataset_name, sort_order, created_at, updated_at
                     FROM records WHERE project_id = ?1 ORDER BY sort_order ASC;",
                )
                .map_err(map_sqlite_err)?;
            let rec_rows = rec_stmt
                .query_map(params![project_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                })
                .map_err(map_sqlite_err)?;

            let mut records = Vec::new();
            for row in rec_rows {
                let row = row.map_err(map_sqlite_err)?;
                records.push(Record {
                    id: row.0,
                    project_id: row.1,
                    dataset_name: row.2,
                    sort_order: to_usize(row.3, "records.sort_order")?,
                    created_at: row.4,
                    updated_at: row.5,
                });
            }

            let mut asset_stmt = conn
                .prepare(
                    "SELECT id, project_id, source_path, file_name, stem, normalized_stem,
                            extension, file_size_bytes, modified_epoch_ms, exists_on_disk
                     FROM assets WHERE project_id = ?1 ORDER BY file_name ASC, source_path ASC;",
                )
                .map_err(map_sqlite_err)?;
            let asset_rows = asset_stmt
                .query_map(params![project_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, i64>(7)?,
                        r.get::<_, i64>(8)?,
                        r.get::<_, i64>(9)?,
                    ))
                })
                .map_err(map_sqlite_err)?;

            let mut assets = Vec::new();
            for row in asset_rows {
                let row = row.map_err(map_sqlite_err)?;
                let format = ImageFormat::from_extension(&row.6).ok_or_else(|| {
                    AppError::database(
                        "ler ativos",
                        format!("Extensão de imagem inválida armazenada no banco: '{}'", row.6),
                    )
                })?;
                assets.push(Asset {
                    id: row.0,
                    project_id: row.1,
                    source_path: PathBuf::from(row.2),
                    file_name: row.3,
                    stem: row.4,
                    normalized_stem: row.5,
                    format,
                    file_size_bytes: to_u64(row.7, "assets.file_size_bytes")?,
                    modified_epoch_ms: row.8,
                    exists_on_disk: row.9 != 0,
                });
            }

            let mut rv_stmt = conn
                .prepare(
                    "SELECT rv.record_id, rv.variable_id, rv.text_value, rv.asset_id,
                            rv.match_status, rv.match_confidence
                     FROM record_values rv
                     INNER JOIN records r ON r.id = rv.record_id
                     WHERE r.project_id = ?1
                     ORDER BY r.sort_order ASC;",
                )
                .map_err(map_sqlite_err)?;
            let rv_rows = rv_stmt
                .query_map(params![project_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, Option<f64>>(5)?,
                    ))
                })
                .map_err(map_sqlite_err)?;

            let mut values = Vec::new();
            for row in rv_rows {
                let row = row.map_err(map_sqlite_err)?;
                values.push(RecordValue {
                    record_id: row.0,
                    variable_id: row.1,
                    text_value: row.2,
                    asset_id: row.3,
                    match_status: MatchStatus::from_db_str(&row.4)?,
                    match_confidence: row.5,
                });
            }

            let mut cand_stmt = conn
                .prepare(
                    "SELECT c.record_id, c.variable_id, c.asset_id, c.score, c.reason
                     FROM photo_match_candidates c
                     INNER JOIN records r ON r.id = c.record_id
                     WHERE r.project_id = ?1
                     ORDER BY c.score DESC;",
                )
                .map_err(map_sqlite_err)?;
            let cand_rows = cand_stmt
                .query_map(params![project_id], |r| {
                    Ok(MatchCandidate {
                        record_id: r.get(0)?,
                        variable_id: r.get(1)?,
                        asset_id: r.get(2)?,
                        score: r.get(3)?,
                        reason: r.get(4)?,
                    })
                })
                .map_err(map_sqlite_err)?;

            let mut match_candidates = Vec::new();
            for row in cand_rows {
                match_candidates.push(row.map_err(map_sqlite_err)?);
            }

            Ok(ProjectSnapshot {
                project,
                variables,
                records,
                values,
                assets,
                match_candidates,
            })
        })
    }

    fn list_projects(&self) -> AppResult<Vec<ProjectSummary>> {
        self.conn_mgr.with_read_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT
                        p.id, p.name, p.mode, p.path_strategy, p.created_at, p.updated_at,
                        (SELECT COUNT(*) FROM variables v WHERE v.project_id = p.id) AS variable_count,
                        (SELECT COUNT(*) FROM records r WHERE r.project_id = p.id) AS record_count,
                        (SELECT COUNT(*) FROM assets a WHERE a.project_id = p.id) AS asset_count
                     FROM projects p
                     ORDER BY p.updated_at DESC, p.created_at DESC;",
                )
                .map_err(map_sqlite_err)?;

            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, i64>(6)?,
                        r.get::<_, i64>(7)?,
                        r.get::<_, i64>(8)?,
                    ))
                })
                .map_err(map_sqlite_err)?;

            let mut out = Vec::new();
            for row in rows {
                let row = row.map_err(map_sqlite_err)?;
                out.push(ProjectSummary {
                    id: row.0,
                    name: row.1,
                    mode: ProjectMode::from_db_str(&row.2)?,
                    path_strategy: PathStrategy::from_db_str(&row.3)?,
                    created_at: row.4,
                    updated_at: row.5,
                    variable_count: to_usize(row.6, "variable_count")?,
                    record_count: to_usize(row.7, "record_count")?,
                    asset_count: to_usize(row.8, "asset_count")?,
                });
            }
            Ok(out)
        })
    }

    fn rename_project(&self, project_id: &str, new_name: &str) -> AppResult<()> {
        let now = utc_now_rfc3339();
        self.conn_mgr.with_immediate_tx(|tx| {
            let changed = tx
                .execute(
                    "UPDATE projects SET name = ?2, updated_at = ?3 WHERE id = ?1;",
                    params![project_id, new_name, now],
                )
                .map_err(map_sqlite_err)?;
            if changed == 0 {
                return Err(AppError::not_found("Projeto", project_id));
            }
            Ok(())
        })
    }

    fn delete_project(&self, project_id: &str) -> AppResult<()> {
        self.conn_mgr.with_immediate_tx(|tx| {
            let changed = tx
                .execute("DELETE FROM projects WHERE id = ?1;", params![project_id])
                .map_err(map_sqlite_err)?;
            if changed == 0 {
                return Err(AppError::not_found("Projeto", project_id));
            }
            tx.execute(
                "DELETE FROM app_settings WHERE key = 'last_opened_project_id' AND value = ?1;",
                params![project_id],
            )
            .map_err(map_sqlite_err)?;
            Ok(())
        })
    }

    fn update_project_settings(&self, project: &Project) -> AppResult<()> {
        let now = utc_now_rfc3339();
        self.conn_mgr.with_immediate_tx(|tx| {
            let changed = tx
                .execute(
                    "UPDATE projects
                     SET name = ?2,
                         mode = ?3,
                         path_strategy = ?4,
                         xml_template_path = ?5,
                         xml_template_raw = ?6,
                         photo_root_dir = ?7,
                         last_export_dir = ?8,
                         updated_at = ?9
                     WHERE id = ?1;",
                    params![
                        project.id,
                        project.name,
                        project.mode.as_db_str(),
                        project.path_strategy.as_db_str(),
                        opt_path_to_str(&project.xml_template_path),
                        project.xml_template_raw,
                        opt_path_to_str(&project.photo_root_dir),
                        opt_path_to_str(&project.last_export_dir),
                        now,
                    ],
                )
                .map_err(map_sqlite_err)?;
            if changed == 0 {
                return Err(AppError::not_found("Projeto", &project.id));
            }
            Ok(())
        })
    }

    fn upsert_variable(&self, var: &Variable) -> AppResult<()> {
        let now = utc_now_rfc3339();
        let sort_order = to_i64(var.sort_order, "ordem da variável")?;
        let required_int: i64 = if var.required { 1 } else { 0 };

        self.conn_mgr.with_immediate_tx(|tx| {
            ensure_project_exists_tx(tx, &var.project_id)?;

            let existing: Option<(String, String)> = tx
                .query_row(
                    "SELECT project_id, kind FROM variables WHERE id = ?1;",
                    params![var.id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(map_sqlite_err)?;

            let kind_switched = if let Some((proj, old_kind)) = existing {
                if proj != var.project_id {
                    return Err(AppError::conflict(format!(
                        "Variável '{}' pertence a outro projeto.",
                        var.id
                    )));
                }
                old_kind != var.kind.as_db_str()
            } else {
                false
            };

            tx.execute(
                "INSERT INTO variables (
                    id, project_id, display_name, xml_name, kind, required, sort_order, mapped_template_var
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(id) DO UPDATE SET
                    display_name = excluded.display_name,
                    xml_name = excluded.xml_name,
                    kind = excluded.kind,
                    required = excluded.required,
                    sort_order = excluded.sort_order,
                    mapped_template_var = excluded.mapped_template_var;",
                params![
                    var.id,
                    var.project_id,
                    var.display_name,
                    var.xml_name,
                    var.kind.as_db_str(),
                    required_int,
                    sort_order,
                    var.mapped_template_var,
                ],
            )
            .map_err(map_sqlite_err)?;

            if kind_switched {
                tx.execute(
                    "UPDATE record_values
                     SET asset_id = NULL,
                         match_status = 'unmatched',
                         match_confidence = NULL
                     WHERE variable_id = ?1;",
                    params![var.id],
                )
                .map_err(map_sqlite_err)?;

                tx.execute(
                    "DELETE FROM photo_match_candidates WHERE variable_id = ?1;",
                    params![var.id],
                )
                .map_err(map_sqlite_err)?;
            }

            touch_project_tx(tx, &var.project_id, &now)?;
            Ok(())
        })
    }

    fn duplicate_variable(&self, project_id: &str, variable_id: &str) -> AppResult<Variable> {
        let now = utc_now_rfc3339();

        self.conn_mgr.with_immediate_tx(|tx| {
            ensure_project_exists_tx(tx, project_id)?;

            let existing_vars = load_variables_tx(tx, project_id)?;
            let source = existing_vars
                .iter()
                .find(|v| v.id == variable_id)
                .ok_or_else(|| AppError::not_found("Variável", variable_id))?;

            let next_sort_order = source.sort_order.checked_add(1).ok_or_else(|| {
                AppError::validation("Limite máximo de ordem atingido ao duplicar variável.")
            })?;
            let new_var = build_duplicated_variable(
                source,
                Uuid::new_v4().to_string(),
                &existing_vars,
                next_sort_order,
            );
            let insert_sort_order = to_i64(new_var.sort_order, "ordem da variável")?;
            let required_int: i64 = if new_var.required { 1 } else { 0 };

            // Deslocar colunas com sort_order >= insert_sort_order em +1 usando duas fases no espaço negativo
            let to_shift: Vec<(String, i64)> = {
                let mut stmt = tx
                    .prepare(
                        "SELECT id, sort_order FROM variables
                         WHERE project_id = ?1 AND sort_order >= ?2
                         ORDER BY sort_order ASC;",
                    )
                    .map_err(map_sqlite_err)?;
                let rows = stmt
                    .query_map(params![project_id, insert_sort_order], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
                    })
                    .map_err(map_sqlite_err)?;
                let mut collected = Vec::new();
                for r in rows {
                    collected.push(r.map_err(map_sqlite_err)?);
                }
                collected
            };

            if !to_shift.is_empty() {
                let min_order: i64 = tx
                    .query_row(
                        "SELECT COALESCE(MIN(sort_order), 0) FROM variables WHERE project_id = ?1;",
                        params![project_id],
                        |r| r.get(0),
                    )
                    .map_err(map_sqlite_err)?;

                {
                    let mut stmt = tx
                        .prepare_cached(
                            "UPDATE variables SET sort_order = ?1 WHERE id = ?2 AND project_id = ?3;",
                        )
                        .map_err(map_sqlite_err)?;
                    for (idx, (id, _)) in to_shift.iter().enumerate() {
                        let temp_order = compute_temp_negative_order(min_order, idx)?;
                        stmt.execute(params![temp_order, id, project_id])
                            .map_err(map_sqlite_err)?;
                    }
                }
                {
                    let mut stmt = tx
                        .prepare_cached(
                            "UPDATE variables SET sort_order = ?1 WHERE id = ?2 AND project_id = ?3;",
                        )
                        .map_err(map_sqlite_err)?;
                    for (id, old_order) in &to_shift {
                        let shifted = old_order.checked_add(1).ok_or_else(|| {
                            AppError::validation(
                                "Limite máximo de ordem excedido ao duplicar variável.",
                            )
                        })?;
                        stmt.execute(params![shifted, id, project_id])
                            .map_err(map_sqlite_err)?;
                    }
                }
            }

            tx.execute(
                "INSERT INTO variables (
                    id, project_id, display_name, xml_name, kind, required, sort_order, mapped_template_var
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8);",
                params![
                    new_var.id,
                    new_var.project_id,
                    new_var.display_name,
                    new_var.xml_name,
                    new_var.kind.as_db_str(),
                    required_int,
                    insert_sort_order,
                    new_var.mapped_template_var,
                ],
            )
            .map_err(map_sqlite_err)?;

            tx.execute(
                "INSERT INTO record_values (
                    record_id, variable_id, text_value, asset_id, match_status, match_confidence
                 )
                 SELECT record_id, ?1, text_value, asset_id, match_status, match_confidence
                 FROM record_values
                 WHERE variable_id = ?2;",
                params![new_var.id, variable_id],
            )
            .map_err(map_sqlite_err)?;

            tx.execute(
                "INSERT INTO photo_match_candidates (
                    record_id, variable_id, asset_id, score, reason
                 )
                 SELECT record_id, ?1, asset_id, score, reason
                 FROM photo_match_candidates
                 WHERE variable_id = ?2;",
                params![new_var.id, variable_id],
            )
            .map_err(map_sqlite_err)?;

            touch_project_tx(tx, project_id, &now)?;
            Ok(new_var)
        })
    }

    fn delete_variable(&self, project_id: &str, variable_id: &str) -> AppResult<()> {
        let now = utc_now_rfc3339();
        self.conn_mgr.with_immediate_tx(|tx| {
            let deleted = tx
                .execute(
                    "DELETE FROM variables WHERE id = ?1 AND project_id = ?2;",
                    params![variable_id, project_id],
                )
                .map_err(map_sqlite_err)?;
            if deleted == 0 {
                return Err(AppError::not_found("Variável", variable_id));
            }

            let remaining: Vec<String> = {
                let mut stmt = tx
                    .prepare(
                        "SELECT id FROM variables WHERE project_id = ?1 ORDER BY sort_order ASC;",
                    )
                    .map_err(map_sqlite_err)?;
                let rows = stmt
                    .query_map(params![project_id], |r| r.get::<_, String>(0))
                    .map_err(map_sqlite_err)?;
                let mut ids = Vec::new();
                for r in rows {
                    ids.push(r.map_err(map_sqlite_err)?);
                }
                ids
            };

            reindex_variables_tx(tx, project_id, &remaining)?;
            touch_project_tx(tx, project_id, &now)?;
            Ok(())
        })
    }

    fn reorder_variables(&self, project_id: &str, ordered_ids: &[String]) -> AppResult<()> {
        let now = utc_now_rfc3339();
        self.conn_mgr.with_immediate_tx(|tx| {
            ensure_project_exists_tx(tx, project_id)?;

            let existing_vars = load_variables_tx(tx, project_id)?;
            let validated = compute_reordered_ids(&existing_vars, ordered_ids)?;
            reindex_variables_tx(tx, project_id, &validated)?;
            touch_project_tx(tx, project_id, &now)?;
            Ok(())
        })
    }

    fn upsert_records_and_values(
        &self,
        project_id: &str,
        records: &[Record],
        values: &[RecordValue],
    ) -> AppResult<()> {
        let now = utc_now_rfc3339();
        self.conn_mgr.with_immediate_tx(|tx| {
            ensure_project_exists_tx(tx, project_id)?;

            {
                let mut rec_stmt = tx
                    .prepare_cached(
                        "INSERT INTO records (id, project_id, dataset_name, sort_order, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                         ON CONFLICT(id) DO UPDATE SET
                             dataset_name = excluded.dataset_name,
                             sort_order = excluded.sort_order,
                             updated_at = excluded.updated_at
                         WHERE records.project_id = excluded.project_id;",
                    )
                    .map_err(map_sqlite_err)?;

                for rec in records {
                    if rec.project_id != project_id {
                        return Err(AppError::validation(format!(
                            "Registro '{}' não pertence ao projeto '{}'.",
                            rec.id, project_id
                        )));
                    }
                    let sort_order = to_i64(rec.sort_order, "ordem do registro")?;
                    let changed = rec_stmt
                        .execute(params![
                            rec.id,
                            rec.project_id,
                            rec.dataset_name,
                            sort_order,
                            rec.created_at,
                            rec.updated_at,
                        ])
                        .map_err(map_sqlite_err)?;
                    if changed == 0 {
                        return Err(AppError::conflict(format!(
                            "Registro '{}' pertence a outro projeto.",
                            rec.id
                        )));
                    }
                }
            }

            {
                let mut verified_records: HashSet<String> = HashSet::new();
                let mut verified_variables: HashSet<String> = HashSet::new();
                let mut verified_assets: HashSet<String> = HashSet::new();

                let mut val_stmt = tx
                    .prepare_cached(
                        "INSERT INTO record_values (
                            record_id, variable_id, text_value, asset_id, match_status, match_confidence
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                         ON CONFLICT(record_id, variable_id) DO UPDATE SET
                            text_value = excluded.text_value,
                            asset_id = excluded.asset_id,
                            match_status = excluded.match_status,
                            match_confidence = excluded.match_confidence;",
                    )
                    .map_err(map_sqlite_err)?;

                for val in values {
                    if !verified_records.contains(&val.record_id) {
                        ensure_entity_in_project_tx(
                            tx,
                            "SELECT project_id FROM records WHERE id = ?1;",
                            "Registro",
                            "Registro pertence a outro projeto",
                            &val.record_id,
                            project_id,
                        )?;
                        verified_records.insert(val.record_id.clone());
                    }
                    if !verified_variables.contains(&val.variable_id) {
                        ensure_entity_in_project_tx(
                            tx,
                            "SELECT project_id FROM variables WHERE id = ?1;",
                            "Variável",
                            "Variável pertence a outro projeto",
                            &val.variable_id,
                            project_id,
                        )?;
                        verified_variables.insert(val.variable_id.clone());
                    }
                    if let Some(ref asset_id) = val.asset_id
                        && !verified_assets.contains(asset_id)
                    {
                        ensure_entity_in_project_tx(
                            tx,
                            "SELECT project_id FROM assets WHERE id = ?1;",
                            "Ativo",
                            "Ativo pertence a outro projeto",
                            asset_id,
                            project_id,
                        )?;
                        verified_assets.insert(asset_id.clone());
                    }

                    val_stmt
                        .execute(params![
                            val.record_id,
                            val.variable_id,
                            val.text_value,
                            val.asset_id,
                            val.match_status.as_db_str(),
                            val.match_confidence,
                        ])
                        .map_err(map_sqlite_err)?;
                }
            }

            touch_project_tx(tx, project_id, &now)?;
            Ok(())
        })
    }

    fn reorder_records(&self, project_id: &str, ordered_ids: &[String]) -> AppResult<()> {
        let now = utc_now_rfc3339();
        self.conn_mgr.with_immediate_tx(|tx| {
            ensure_project_exists_tx(tx, project_id)?;

            let existing_records = load_records_tx(tx, project_id)?;
            let validated = compute_reordered_record_ids(&existing_records, ordered_ids)?;
            reindex_records_tx(tx, project_id, &validated)?;
            touch_project_tx(tx, project_id, &now)?;
            Ok(())
        })
    }

    fn delete_records(&self, project_id: &str, record_ids: &[String]) -> AppResult<()> {
        let now = utc_now_rfc3339();
        self.conn_mgr.with_immediate_tx(|tx| {
            ensure_project_exists_tx(tx, project_id)?;
            if record_ids.is_empty() {
                return Ok(());
            }

            {
                let mut stmt = tx
                    .prepare_cached("DELETE FROM records WHERE id = ?1 AND project_id = ?2;")
                    .map_err(map_sqlite_err)?;
                for id in record_ids {
                    stmt.execute(params![id, project_id])
                        .map_err(map_sqlite_err)?;
                }
            }

            let remaining: Vec<String> = {
                let mut stmt = tx
                    .prepare("SELECT id FROM records WHERE project_id = ?1 ORDER BY sort_order ASC;")
                    .map_err(map_sqlite_err)?;
                let rows = stmt
                    .query_map(params![project_id], |r| r.get::<_, String>(0))
                    .map_err(map_sqlite_err)?;
                let mut ids = Vec::new();
                for r in rows {
                    ids.push(r.map_err(map_sqlite_err)?);
                }
                ids
            };

            reindex_records_tx(tx, project_id, &remaining)?;
            touch_project_tx(tx, project_id, &now)?;
            Ok(())
        })
    }

    fn replace_assets(&self, project_id: &str, assets: &[Asset]) -> AppResult<()> {
        let now = utc_now_rfc3339();
        self.conn_mgr.with_immediate_tx(|tx| {
            ensure_project_exists_tx(tx, project_id)?;

            for asset in assets {
                if asset.project_id != project_id {
                    return Err(AppError::validation(format!(
                        "Ativo '{}' não pertence ao projeto '{}'.",
                        asset.id, project_id
                    )));
                }
            }

            let keep_ids: HashSet<&str> = assets.iter().map(|a| a.id.as_str()).collect();
            let existing_ids: Vec<String> = {
                let mut stmt = tx
                    .prepare("SELECT id FROM assets WHERE project_id = ?1;")
                    .map_err(map_sqlite_err)?;
                let rows = stmt
                    .query_map(params![project_id], |r| r.get::<_, String>(0))
                    .map_err(map_sqlite_err)?;
                let mut ids = Vec::new();
                for r in rows {
                    ids.push(r.map_err(map_sqlite_err)?);
                }
                ids
            };

            {
                let mut del_stmt = tx
                    .prepare_cached("DELETE FROM assets WHERE id = ?1 AND project_id = ?2;")
                    .map_err(map_sqlite_err)?;
                for id in existing_ids {
                    if !keep_ids.contains(id.as_str()) {
                        del_stmt
                            .execute(params![id, project_id])
                            .map_err(map_sqlite_err)?;
                    }
                }
            }

            {
                let mut upsert_stmt = tx
                    .prepare_cached(
                        "INSERT INTO assets (
                            id, project_id, source_path, file_name, stem, normalized_stem,
                            extension, file_size_bytes, modified_epoch_ms, exists_on_disk
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                         ON CONFLICT(id) DO UPDATE SET
                            source_path = excluded.source_path,
                            file_name = excluded.file_name,
                            stem = excluded.stem,
                            normalized_stem = excluded.normalized_stem,
                            extension = excluded.extension,
                            file_size_bytes = excluded.file_size_bytes,
                            modified_epoch_ms = excluded.modified_epoch_ms,
                            exists_on_disk = excluded.exists_on_disk
                         WHERE assets.project_id = excluded.project_id;",
                    )
                    .map_err(map_sqlite_err)?;

                for asset in assets {
                    let size_i64 = i64::try_from(asset.file_size_bytes).map_err(|_| {
                        AppError::validation(format!(
                            "Tamanho de arquivo excede limite i64: {}",
                            asset.file_size_bytes
                        ))
                    })?;
                    let exists_int: i64 = if asset.exists_on_disk { 1 } else { 0 };
                    let changed = upsert_stmt
                        .execute(params![
                            asset.id,
                            asset.project_id,
                            asset.source_path.to_string_lossy().into_owned(),
                            asset.file_name,
                            asset.stem,
                            asset.normalized_stem,
                            asset.format.canonical_ext(),
                            size_i64,
                            asset.modified_epoch_ms,
                            exists_int,
                        ])
                        .map_err(map_sqlite_err)?;
                    if changed == 0 {
                        return Err(AppError::conflict(format!(
                            "Ativo '{}' pertence a outro projeto.",
                            asset.id
                        )));
                    }
                }
            }

            touch_project_tx(tx, project_id, &now)?;
            Ok(())
        })
    }

    fn update_match_candidates(
        &self,
        project_id: &str,
        candidates: &[MatchCandidate],
    ) -> AppResult<()> {
        let now = utc_now_rfc3339();
        self.conn_mgr.with_immediate_tx(|tx| {
            ensure_project_exists_tx(tx, project_id)?;

            let mut verified_records: HashSet<String> = HashSet::new();
            let mut verified_variables: HashSet<String> = HashSet::new();
            let mut verified_assets: HashSet<String> = HashSet::new();

            for cand in candidates {
                if !verified_records.contains(&cand.record_id) {
                    ensure_entity_in_project_tx(
                        tx,
                        "SELECT project_id FROM records WHERE id = ?1;",
                        "Registro",
                        "Registro pertence a outro projeto",
                        &cand.record_id,
                        project_id,
                    )?;
                    verified_records.insert(cand.record_id.clone());
                }
                if !verified_variables.contains(&cand.variable_id) {
                    ensure_entity_in_project_tx(
                        tx,
                        "SELECT project_id FROM variables WHERE id = ?1;",
                        "Variável",
                        "Variável pertence a outro projeto",
                        &cand.variable_id,
                        project_id,
                    )?;
                    verified_variables.insert(cand.variable_id.clone());
                }
                if !verified_assets.contains(&cand.asset_id) {
                    ensure_entity_in_project_tx(
                        tx,
                        "SELECT project_id FROM assets WHERE id = ?1;",
                        "Ativo",
                        "Ativo pertence a outro projeto",
                        &cand.asset_id,
                        project_id,
                    )?;
                    verified_assets.insert(cand.asset_id.clone());
                }
            }

            tx.execute(
                "DELETE FROM photo_match_candidates
                 WHERE record_id IN (SELECT id FROM records WHERE project_id = ?1);",
                params![project_id],
            )
            .map_err(map_sqlite_err)?;

            {
                let mut stmt = tx
                    .prepare_cached(
                        "INSERT INTO photo_match_candidates (record_id, variable_id, asset_id, score, reason)
                         VALUES (?1, ?2, ?3, ?4, ?5)
                         ON CONFLICT(record_id, variable_id, asset_id) DO UPDATE SET
                             score = excluded.score,
                             reason = excluded.reason;",
                    )
                    .map_err(map_sqlite_err)?;

                for cand in candidates {
                    stmt.execute(params![
                        cand.record_id,
                        cand.variable_id,
                        cand.asset_id,
                        cand.score,
                        cand.reason,
                    ])
                    .map_err(map_sqlite_err)?;
                }
            }

            touch_project_tx(tx, project_id, &now)?;
            Ok(())
        })
    }

    fn get_setting(&self, key: &str) -> AppResult<Option<String>> {
        self.conn_mgr.with_read_conn(|conn| {
            conn.query_row(
                "SELECT value FROM app_settings WHERE key = ?1;",
                params![key],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(map_sqlite_err)
        })
    }

    fn set_setting(&self, key: &str, value: &str) -> AppResult<()> {
        self.conn_mgr.with_immediate_tx(|tx| {
            tx.execute(
                "INSERT INTO app_settings (key, value)
                 VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value;",
                params![key, value],
            )
            .map_err(map_sqlite_err)?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::sqlite::migrations::run_migrations;
    use crate::shared::error::DomainError;

    fn setup_repo() -> SqliteProjectRepository {
        let mgr = SqliteConnectionManager::open_in_memory().unwrap();
        mgr.with_conn_mut(run_migrations).unwrap();
        SqliteProjectRepository::new(mgr)
    }

    #[test]
    fn migrations_are_idempotent_and_foreign_keys_enabled() {
        let repo = setup_repo();
        repo.conn_mgr().with_conn_mut(run_migrations).unwrap();

        let (fk, count): (i64, i64) = repo
            .conn_mgr()
            .with_read_conn(|conn| {
                let fk: i64 = conn
                    .query_row("PRAGMA foreign_keys;", [], |r| r.get(0))
                    .map_err(map_sqlite_err)?;
                let count: i64 = conn
                    .query_row("SELECT COUNT(*) FROM schema_migrations;", [], |r| r.get(0))
                    .map_err(map_sqlite_err)?;
                Ok((fk, count))
            })
            .unwrap();

        assert_eq!(fk, 1);
        assert_eq!(count, 1);
    }

    #[test]
    fn reorder_variables_rotates_and_reverses_without_unique_collision() {
        let repo = setup_repo();
        let proj = repo
            .create_project("Projeto Teste", ProjectMode::WithoutTemplate)
            .unwrap();

        let vars: Vec<Variable> = ["A", "B", "C", "D"]
            .iter()
            .enumerate()
            .map(|(i, name)| Variable {
                id: format!("v{i}"),
                project_id: proj.id.clone(),
                display_name: (*name).to_string(),
                xml_name: (*name).to_string(),
                kind: VariableKind::Text,
                required: false,
                sort_order: i,
                mapped_template_var: None,
            })
            .collect();
        for v in &vars {
            repo.upsert_variable(v).unwrap();
        }

        // Rotação [A, B, C, D] -> [C, A, D, B]
        repo.reorder_variables(
            &proj.id,
            &["v2".into(), "v0".into(), "v3".into(), "v1".into()],
        )
        .unwrap();
        let snap1 = repo.open_project(&proj.id).unwrap();
        let ids1: Vec<&str> = snap1.variables.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids1, vec!["v2", "v0", "v3", "v1"]);

        // Reversão completa
        repo.reorder_variables(
            &proj.id,
            &["v1".into(), "v3".into(), "v0".into(), "v2".into()],
        )
        .unwrap();
        let snap2 = repo.open_project(&proj.id).unwrap();
        let ids2: Vec<&str> = snap2.variables.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids2, vec!["v1", "v3", "v0", "v2"]);

        // Permutação inválida deve falhar e manter ordem anterior
        let err = repo
            .reorder_variables(&proj.id, &["v1".into(), "v3".into()])
            .unwrap_err();
        assert!(matches!(err, AppError::Domain(DomainError::Validation(_))));
        let snap3 = repo.open_project(&proj.id).unwrap();
        let ids3: Vec<&str> = snap3.variables.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids3, vec!["v1", "v3", "v0", "v2"]);
    }

    #[test]
    fn upsert_variable_update_preserves_existing_record_values() {
        let repo = setup_repo();
        let proj = repo
            .create_project("Crachás", ProjectMode::WithoutTemplate)
            .unwrap();

        let mut var = Variable {
            id: "v1".into(),
            project_id: proj.id.clone(),
            display_name: "Matrícula".into(),
            xml_name: "Matricula".into(),
            kind: VariableKind::Text,
            required: true,
            sort_order: 0,
            mapped_template_var: None,
        };
        repo.upsert_variable(&var).unwrap();

        let rec = Record {
            id: "r1".into(),
            project_id: proj.id.clone(),
            dataset_name: "DS_01".into(),
            sort_order: 0,
            created_at: "2026-11-17T00:00:00.000Z".into(),
            updated_at: "2026-11-17T00:00:00.000Z".into(),
        };
        let val = RecordValue::new_text("r1", "v1", "00421");
        repo.upsert_records_and_values(&proj.id, &[rec], &[val])
            .unwrap();

        var.display_name = "Código".into();
        var.xml_name = "Codigo".into();
        repo.upsert_variable(&var).unwrap();

        let snap = repo.open_project(&proj.id).unwrap();
        assert_eq!(snap.variables[0].display_name, "Código");
        assert_eq!(snap.value_for("r1", "v1").unwrap().text_value, "00421");
    }

    #[test]
    fn reorder_records_rotates_and_reverses_without_unique_collision() {
        let repo = setup_repo();
        let proj = repo
            .create_project("Reordenar Registros", ProjectMode::WithoutTemplate)
            .unwrap();

        let records: Vec<Record> = (0..4)
            .map(|i| Record {
                id: format!("r{i}"),
                project_id: proj.id.clone(),
                dataset_name: format!("DS_{i}"),
                sort_order: i,
                created_at: "2026-11-17T00:00:00.000Z".into(),
                updated_at: "2026-11-17T00:00:00.000Z".into(),
            })
            .collect();
        repo.upsert_records_and_values(&proj.id, &records, &[])
            .unwrap();

        repo.reorder_records(
            &proj.id,
            &["r2".into(), "r0".into(), "r3".into(), "r1".into()],
        )
        .unwrap();
        let snap1 = repo.open_project(&proj.id).unwrap();
        let ids1: Vec<&str> = snap1.records.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids1, vec!["r2", "r0", "r3", "r1"]);

        repo.reorder_records(
            &proj.id,
            &["r1".into(), "r3".into(), "r0".into(), "r2".into()],
        )
        .unwrap();
        let snap2 = repo.open_project(&proj.id).unwrap();
        let ids2: Vec<&str> = snap2.records.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids2, vec!["r1", "r3", "r0", "r2"]);

        let err = repo
            .reorder_records(&proj.id, &["r1".into(), "r3".into()])
            .unwrap_err();
        assert!(matches!(err, AppError::Domain(DomainError::Validation(_))));
        let snap3 = repo.open_project(&proj.id).unwrap();
        let ids3: Vec<&str> = snap3.records.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids3, vec!["r1", "r3", "r0", "r2"]);
    }

    #[test]
    fn duplicate_variable_in_repo_copies_eav_and_shifts_adjacent_order() {
        let repo = setup_repo();
        let proj = repo
            .create_project("Duplicar Direto", ProjectMode::WithoutTemplate)
            .unwrap();

        let v0 = Variable {
            id: "v0".into(),
            project_id: proj.id.clone(),
            display_name: "Matrícula".into(),
            xml_name: "Matricula".into(),
            kind: VariableKind::Text,
            required: true,
            sort_order: 0,
            mapped_template_var: Some("TplVar".into()),
        };
        let v1 = Variable {
            id: "v1".into(),
            project_id: proj.id.clone(),
            display_name: "Cargo".into(),
            xml_name: "Cargo".into(),
            kind: VariableKind::Text,
            required: false,
            sort_order: 1,
            mapped_template_var: None,
        };
        repo.upsert_variable(&v0).unwrap();
        repo.upsert_variable(&v1).unwrap();

        let rec = Record {
            id: "r0".into(),
            project_id: proj.id.clone(),
            dataset_name: "DS_0".into(),
            sort_order: 0,
            created_at: "2026-11-17T00:00:00.000Z".into(),
            updated_at: "2026-11-17T00:00:00.000Z".into(),
        };
        let val = RecordValue::new_text("r0", "v0", "00421");
        repo.upsert_records_and_values(&proj.id, &[rec], &[val])
            .unwrap();

        let dup = repo.duplicate_variable(&proj.id, "v0").unwrap();
        assert_eq!(dup.xml_name, "Matricula_copia");
        assert_eq!(dup.display_name, "Matrícula (Cópia)");
        assert_eq!(dup.sort_order, 1);
        assert_eq!(dup.mapped_template_var, None);

        let snap = repo.open_project(&proj.id).unwrap();
        let ordered: Vec<(&str, usize)> = snap
            .variables
            .iter()
            .map(|v| (v.xml_name.as_str(), v.sort_order))
            .collect();
        assert_eq!(
            ordered,
            vec![("Matricula", 0), ("Matricula_copia", 1), ("Cargo", 2)]
        );
        assert_eq!(snap.value_for("r0", &dup.id).unwrap().text_value, "00421");
    }
}
