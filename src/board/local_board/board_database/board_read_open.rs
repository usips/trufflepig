//! Read-only database opening for query paths that never create or migrate.

use super::{SCHEMA_VERSION, io_error, sql_error, unavailable};
use crate::board::board_protocol::{BoardError, BoardErrorCode};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A query-only connection refuses absent or unmigrated storage and never
/// creates, migrates, or chmods it: read paths leave the filesystem untouched.
pub(in crate::board::local_board) fn open_read_with_timeout(
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
        return Err(BoardError::new(
            BoardErrorCode::BoardInitializationRequired,
            format!(
                "schema version {version} requires writable initialization for supported {SCHEMA_VERSION}"
            ),
        ));
    }
    if version > SCHEMA_VERSION {
        return Err(BoardError::new(
            BoardErrorCode::SchemaNewer,
            format!("schema version {version} is newer than supported {SCHEMA_VERSION}"),
        ));
    }
    Ok((conn, resolved))
}
