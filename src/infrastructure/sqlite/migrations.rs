use rusqlite::{Connection, TransactionBehavior, params};

use crate::infrastructure::sqlite::connection::map_sqlite_err;
use crate::shared::error::AppResult;

const MIGRATION_V1_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS projects (
    id                  TEXT PRIMARY KEY,
    name                TEXT NOT NULL,
    mode                TEXT NOT NULL CHECK (mode IN ('with_template', 'without_template')),
    path_strategy       TEXT NOT NULL DEFAULT 'relative_to_xml'
                        CHECK (path_strategy IN ('relative_to_xml', 'absolute_file_uri')),
    xml_template_path   TEXT,
    xml_template_raw    TEXT,
    photo_root_dir      TEXT,
    last_export_dir     TEXT,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS variables (
    id                  TEXT PRIMARY KEY,
    project_id          TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    display_name        TEXT NOT NULL,
    xml_name            TEXT NOT NULL,
    kind                TEXT NOT NULL CHECK (kind IN ('text', 'linked_file')),
    required            INTEGER NOT NULL DEFAULT 0 CHECK (required IN (0, 1)),
    sort_order          INTEGER NOT NULL,
    mapped_template_var TEXT,
    UNIQUE(project_id, xml_name),
    UNIQUE(project_id, sort_order)
);

CREATE TABLE IF NOT EXISTS records (
    id                  TEXT PRIMARY KEY,
    project_id          TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    dataset_name        TEXT NOT NULL,
    sort_order          INTEGER NOT NULL,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    UNIQUE(project_id, sort_order)
);

CREATE TABLE IF NOT EXISTS assets (
    id                  TEXT PRIMARY KEY,
    project_id          TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    source_path         TEXT NOT NULL,
    file_name           TEXT NOT NULL,
    stem                TEXT NOT NULL,
    normalized_stem     TEXT NOT NULL,
    extension           TEXT NOT NULL,
    file_size_bytes     INTEGER NOT NULL,
    modified_epoch_ms   INTEGER NOT NULL,
    exists_on_disk      INTEGER NOT NULL DEFAULT 1 CHECK (exists_on_disk IN (0, 1)),
    UNIQUE(project_id, source_path)
);

CREATE TABLE IF NOT EXISTS record_values (
    record_id           TEXT NOT NULL REFERENCES records(id) ON DELETE CASCADE,
    variable_id         TEXT NOT NULL REFERENCES variables(id) ON DELETE CASCADE,
    text_value          TEXT NOT NULL DEFAULT '',
    asset_id            TEXT REFERENCES assets(id) ON DELETE SET NULL,
    match_status        TEXT NOT NULL DEFAULT 'unmatched'
                        CHECK (match_status IN ('unmatched', 'auto_exact', 'auto_suggested', 'ambiguous', 'manual')),
    match_confidence    REAL,
    PRIMARY KEY (record_id, variable_id)
);

CREATE TABLE IF NOT EXISTS photo_match_candidates (
    record_id           TEXT NOT NULL REFERENCES records(id) ON DELETE CASCADE,
    variable_id         TEXT NOT NULL REFERENCES variables(id) ON DELETE CASCADE,
    asset_id            TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    score               REAL NOT NULL,
    reason              TEXT NOT NULL,
    PRIMARY KEY (record_id, variable_id, asset_id)
);

CREATE TABLE IF NOT EXISTS app_settings (
    key                 TEXT PRIMARY KEY,
    value               TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_variables_project ON variables(project_id, sort_order);
CREATE INDEX IF NOT EXISTS idx_records_project ON records(project_id, sort_order);
CREATE INDEX IF NOT EXISTS idx_assets_norm_stem ON assets(project_id, normalized_stem);
CREATE INDEX IF NOT EXISTS idx_rv_asset ON record_values(asset_id);
"#;

const MIGRATIONS: &[(i64, &str)] = &[(1, MIGRATION_V1_SQL)];

pub fn run_migrations(conn: &mut Connection) -> AppResult<()> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(map_sqlite_err)?;

    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version     INTEGER PRIMARY KEY,
             applied_at  TEXT NOT NULL
         );",
    )
    .map_err(map_sqlite_err)?;

    let current_version: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations;",
            [],
            |row| row.get(0),
        )
        .map_err(map_sqlite_err)?;

    for &(version, sql) in MIGRATIONS {
        if version > current_version {
            tx.execute_batch(sql).map_err(map_sqlite_err)?;
            tx.execute(
                "INSERT INTO schema_migrations (version, applied_at)
                 VALUES (?1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));",
                params![version],
            )
            .map_err(map_sqlite_err)?;
        }
    }

    tx.commit().map_err(map_sqlite_err)?;
    Ok(())
}
