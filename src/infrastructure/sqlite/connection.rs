use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, ErrorCode, Transaction, TransactionBehavior};

use crate::shared::error::{AppError, AppResult};

pub struct SqliteConnectionManager {
    conn: Mutex<Connection>,
    db_path: PathBuf,
}

impl SqliteConnectionManager {
    pub fn open(db_path: &Path) -> AppResult<Self> {
        if let Some(parent) = db_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| {
                AppError::io(format!("criar diretório do banco '{}'", parent.display()), e)
            })?;
        }
        let conn = Connection::open(db_path).map_err(map_sqlite_err)?;
        configure_pragmas(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            db_path: db_path.to_path_buf(),
        })
    }

    pub fn open_in_memory() -> AppResult<Self> {
        let conn = Connection::open_in_memory().map_err(map_sqlite_err)?;
        configure_pragmas(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            db_path: PathBuf::from(":memory:"),
        })
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    pub fn with_conn_mut<T, F>(&self, f: F) -> AppResult<T>
    where
        F: FnOnce(&mut Connection) -> AppResult<T>,
    {
        let mut guard = self.conn.lock().map_err(|_| {
            AppError::database("mutex", "Lock do banco SQLite corrompido (poisoned)")
        })?;
        f(&mut guard)
    }

    pub fn with_read_conn<T, F>(&self, f: F) -> AppResult<T>
    where
        F: FnOnce(&Connection) -> AppResult<T>,
    {
        let guard = self.conn.lock().map_err(|_| {
            AppError::database("mutex", "Lock do banco SQLite corrompido (poisoned)")
        })?;
        f(&guard)
    }

    pub fn with_immediate_tx<T, F>(&self, f: F) -> AppResult<T>
    where
        F: FnOnce(&Transaction<'_>) -> AppResult<T>,
    {
        let mut guard = self.conn.lock().map_err(|_| {
            AppError::database("mutex", "Lock do banco SQLite corrompido (poisoned)")
        })?;
        let tx = guard
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite_err)?;
        let out = f(&tx)?;
        tx.commit().map_err(map_sqlite_err)?;
        Ok(out)
    }
}

fn is_sqlite_busy(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(sqlite_err, _)
            if matches!(
                sqlite_err.code,
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
            )
    )
}

fn configure_pragmas(conn: &Connection) -> AppResult<()> {
    conn.busy_timeout(std::time::Duration::from_millis(5000))
        .map_err(map_sqlite_err)?;
    conn.execute_batch(
        "PRAGMA busy_timeout = 5000;
         PRAGMA foreign_keys = ON;
         PRAGMA synchronous = NORMAL;",
    )
    .map_err(map_sqlite_err)?;

    // Sob aberturas simultâneas em arquivo novo, o switch DELETE -> WAL no SQLite pode retornar
    // SQLITE_BUSY imediatamente sem acionar busy_timeout para evitar deadlock de upgrade de lock.
    let mut last_err = None;
    for attempt in 0..20u64 {
        match conn.execute_batch("PRAGMA journal_mode = WAL;") {
            Ok(()) => {
                last_err = None;
                break;
            }
            Err(err) if is_sqlite_busy(&err) && attempt < 19 => {
                last_err = Some(err);
                std::thread::sleep(std::time::Duration::from_millis(10 + (attempt * 5)));
            }
            Err(err) => return Err(map_sqlite_err(err)),
        }
    }
    if let Some(err) = last_err {
        return Err(map_sqlite_err(err));
    }

    let fk_enabled: i64 = conn
        .query_row("PRAGMA foreign_keys;", [], |r| r.get(0))
        .map_err(map_sqlite_err)?;
    if fk_enabled != 1 {
        return Err(AppError::database(
            "configurar pragmas",
            "Não foi possível ativar PRAGMA foreign_keys = ON no SQLite",
        ));
    }
    Ok(())
}

pub fn map_sqlite_err(err: rusqlite::Error) -> AppError {
    match &err {
        rusqlite::Error::QueryReturnedNoRows => AppError::not_found("Registro", "consulta"),
        rusqlite::Error::SqliteFailure(sqlite_err, detail) => {
            let msg = detail.as_deref().unwrap_or("Erro SQLite");
            match sqlite_err.code {
                ErrorCode::ConstraintViolation => {
                    tracing::debug!(
                        code = ?sqlite_err.code,
                        extended_code = sqlite_err.extended_code,
                        detail = msg,
                        "Violação de restrição SQLite convertida para erro de domínio"
                    );
                    map_constraint_violation(sqlite_err.extended_code, msg)
                }
                _ => AppError::database("SQLite", format!("{:?}: {msg}", sqlite_err.code)),
            }
        }
        _ => AppError::database("SQLite", err.to_string()),
    }
}

fn map_constraint_violation(extended_code: std::ffi::c_int, msg: &str) -> AppError {
    if msg.contains("UNIQUE")
        || extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
        || extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
    {
        if msg.contains("variables.xml_name") {
            AppError::conflict(
                "O identificador XML já está em uso por outra coluna neste projeto.",
            )
        } else if msg.contains("variables.sort_order") {
            AppError::conflict(
                "A ordem da coluna entrou em conflito com outra coluna existente no projeto.",
            )
        } else if msg.contains("records.sort_order") {
            AppError::conflict(
                "A ordem do registro entrou em conflito com outro registro existente no projeto.",
            )
        } else if msg.contains("assets.source_path") {
            AppError::conflict(
                "Já existe uma fotografia cadastrada com este caminho neste projeto.",
            )
        } else {
            AppError::conflict("Já existe um item com os mesmos dados únicos neste projeto.")
        }
    } else if msg.contains("FOREIGN KEY")
        || extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY
    {
        AppError::validation(
            "Referência inválida para projeto, coluna, registro ou fotografia inexistente.",
        )
    } else if msg.contains("NOT NULL")
        || extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_NOTNULL
    {
        AppError::validation("Um campo obrigatório não foi preenchido.")
    } else if msg.contains("CHECK") || extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_CHECK {
        AppError::validation("Um dos valores informados é inválido para o campo especificado.")
    } else {
        AppError::validation("Os dados informados violam uma regra de integridade.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_no_sqlite_leak(err: &AppError) {
        let user_msg = err.user_message();
        assert!(!user_msg.trim().is_empty());
        for forbidden in [
            "UNIQUE constraint",
            "CHECK constraint",
            "FOREIGN KEY",
            "NOT NULL constraint",
            "variables.",
            "records.",
            "assets.",
            "SQLITE_",
        ] {
            assert!(
                !user_msg.contains(forbidden),
                "user_message() vazou '{forbidden}': {user_msg}"
            );
        }
    }

    #[test]
    fn map_sqlite_err_sanitizes_all_constraint_violations() {
        let cases = [
            (
                rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE,
                "UNIQUE constraint failed: variables.project_id, variables.xml_name",
                true,
            ),
            (
                rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE,
                "UNIQUE constraint failed: variables.project_id, variables.sort_order",
                true,
            ),
            (
                rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE,
                "UNIQUE constraint failed: records.project_id, records.sort_order",
                true,
            ),
            (
                rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE,
                "UNIQUE constraint failed: assets.project_id, assets.source_path",
                true,
            ),
            (
                rusqlite::ffi::SQLITE_CONSTRAINT_CHECK,
                "CHECK constraint failed: kind IN ('text', 'linked_file')",
                false,
            ),
            (
                rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY,
                "FOREIGN KEY constraint failed",
                false,
            ),
            (
                rusqlite::ffi::SQLITE_CONSTRAINT_NOTNULL,
                "NOT NULL constraint failed: variables.display_name",
                false,
            ),
        ];

        for (ext_code, detail, expect_conflict) in cases {
            let err = map_sqlite_err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error {
                    code: ErrorCode::ConstraintViolation,
                    extended_code: ext_code,
                },
                Some(detail.to_string()),
            ));
            assert_no_sqlite_leak(&err);
            if expect_conflict {
                assert!(matches!(
                    err,
                    AppError::Domain(crate::shared::error::DomainError::Conflict(_))
                ));
            } else {
                assert!(matches!(
                    err,
                    AppError::Domain(crate::shared::error::DomainError::Validation(_))
                ));
            }
        }
    }
}
