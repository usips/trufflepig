//! Secure durable database creation and forward-only schema migrations.

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags};

use super::{BoardError, invalid, sql_error};

mod board_schema;

use board_schema::{SCHEMA_V1, SCHEMA_V2, SCHEMA_VERSION};

#[cfg(test)]
pub(super) fn open(path: &Path) -> Result<(Connection, PathBuf), BoardError> {
    open_with_timeout(path, Duration::from_secs(5))
}

pub(super) fn open_with_timeout(
    path: &Path,
    timeout: Duration,
) -> Result<(Connection, PathBuf), BoardError> {
    let deadline = Instant::now() + timeout.min(Duration::from_secs(5));
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .or_else(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    Ok(())
                } else {
                    Err(e)
                }
            })
            .map_err(io_error)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(parent).map_err(io_error)?;
    let parent = parent.canonicalize().map_err(io_error)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = parent.metadata().map_err(io_error)?.permissions().mode();
        if mode & 0o200 == 0 {
            return Err(unavailable("database directory is not writable"));
        }
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700))
            .map_err(io_error)?;
        if parent.metadata().map_err(io_error)?.permissions().mode() & 0o777 != 0o700 {
            return Err(unavailable("database directory permissions must be 0700"));
        }
    }
    let name = path
        .file_name()
        .ok_or_else(|| unavailable("database path has no filename"))?;
    let resolved = parent.join(name);
    if resolved
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(unavailable("database path is a symbolic link"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&resolved) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = resolved.metadata().map_err(io_error)?.permissions().mode();
                if mode & 0o077 != 0 {
                    return Err(unavailable("database file permissions must be 0600"));
                }
                if mode & 0o200 == 0 {
                    return Err(unavailable("database file is not writable"));
                }
            }
        }
        Err(error) => return Err(io_error(error)),
    }
    let mut conn = Connection::open(&resolved).map_err(sql_error)?;
    let version: i64 = retry_busy(&conn, deadline, || {
        conn.query_row("PRAGMA user_version", [], |r| r.get(0))
    })?;
    if version > SCHEMA_VERSION {
        return Err(unavailable(format!(
            "schema version {version} is newer than supported {SCHEMA_VERSION}"
        )));
    }
    let journal: String = retry_busy(&conn, deadline, || {
        conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
    })?;
    if !journal.eq_ignore_ascii_case("wal") {
        return Err(unavailable("database cannot enable WAL journal mode"));
    }
    retry_busy(&conn, deadline, || {
        conn.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")
    })?;
    conn.busy_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(sql_error)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(sql_error)?;
    let locked_version: i64 = tx
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(sql_error)?;
    if locked_version > SCHEMA_VERSION {
        return Err(unavailable("database schema became newer than supported"));
    }
    for version in locked_version..SCHEMA_VERSION {
        let migration = match version {
            0 => SCHEMA_V1,
            1 => SCHEMA_V2,
            _ => {
                return Err(unavailable(format!(
                    "missing schema migration from version {version}"
                )));
            }
        };
        tx.execute_batch(migration).map_err(sql_error)?;
        tx.pragma_update(None, "user_version", version + 1)
            .map_err(sql_error)?;
    }
    tx.execute("INSERT INTO board_meta(key,value) VALUES('resolved_path',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [resolved.to_string_lossy().as_ref()]).map_err(sql_error)?;
    tx.commit().map_err(sql_error)?;
    conn.busy_timeout(timeout.min(Duration::from_secs(5)))
        .map_err(sql_error)?;
    Ok((conn, resolved))
}

/// A query-only connection refuses absent or unmigrated storage and never creates it.
pub(super) fn open_read_with_timeout(
    path: &Path,
    timeout: Duration,
) -> Result<(Connection, PathBuf), BoardError> {
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(unavailable("database path is a symbolic link"));
    }
    let resolved = path.canonicalize().map_err(io_error)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let parent = resolved
            .parent()
            .ok_or_else(|| unavailable("database has no parent"))?;
        let mode = parent.metadata().map_err(io_error)?.permissions().mode();
        if mode & 0o077 != 0 {
            // Preserve a deliberately read-only owner's mode while removing other access.
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(mode & !0o077))
                .map_err(io_error)?;
        }
    }
    let conn = Connection::open_with_flags(&resolved, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(sql_error)?;
    conn.busy_timeout(timeout.min(Duration::from_secs(5)))
        .map_err(sql_error)?;
    conn.execute_batch("PRAGMA query_only=ON; PRAGMA foreign_keys=ON;")
        .map_err(sql_error)?;
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(sql_error)?;
    if version < SCHEMA_VERSION {
        return Err(super::BoardError::new(
            super::BoardErrorCode::BoardInitializationRequired,
            format!(
                "schema version {version} requires writable initialization for supported {SCHEMA_VERSION}"
            ),
        ));
    }
    if version > SCHEMA_VERSION {
        return Err(unavailable(format!(
            "schema version {version} is newer than supported {SCHEMA_VERSION}"
        )));
    }
    Ok((conn, resolved))
}

fn retry_busy<T>(
    conn: &Connection,
    deadline: Instant,
    mut operation: impl FnMut() -> rusqlite::Result<T>,
) -> Result<T, BoardError> {
    loop {
        conn.busy_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(sql_error)?;
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) => {
                let busy = matches!(&error, rusqlite::Error::SqliteFailure(code, _) if matches!(code.code,rusqlite::ErrorCode::DatabaseBusy|rusqlite::ErrorCode::DatabaseLocked));
                let remaining = deadline.saturating_duration_since(Instant::now());
                if !busy || remaining.is_zero() {
                    return Err(sql_error(error));
                }
                // Journal-mode lock promotion can fail without invoking SQLite's busy handler.
                std::thread::sleep(remaining.min(Duration::from_millis(1)));
            }
        }
    }
}

fn io_error(error: std::io::Error) -> BoardError {
    unavailable(error.to_string())
}

fn unavailable(message: impl Into<String>) -> BoardError {
    invalid("board_unavailable", message)
}

#[cfg(test)]
pub(super) fn legacy_schema() -> &'static str {
    SCHEMA_V1
}
