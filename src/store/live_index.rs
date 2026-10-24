//! The writer's `index.sqlite3`: pragmas, schema revision, corruption
//! quarantine, and space reclamation after publication. The index is a
//! rebuildable cache, so another revision or a corrupt file is replaced, never
//! migrated ([storage contract](../../docs/index-storage-contract.md)).
use super::{cache_access, encode_path, index_deadline, publish, schema};
use crate::daemon::deadline::QueryDeadline;
use anyhow::{Context, Result};
use rusqlite::{
    Connection, Error as SqliteError, ErrorCode, OptionalExtension, Transaction,
    TransactionBehavior,
};
use std::path::Path;
use std::time::Duration;

pub(super) const INDEX_NAME: &str = "index.sqlite3";
/// The most recent corrupt index, kept for inspection; each quarantine replaces it.
const QUARANTINE_NAME: &str = "index.corrupt.sqlite3";
/// Auto-vacuum applies only before an empty file's first write, so it precedes
/// WAL; its header write needs no sync. Then the writer page cache, and the WAL
/// size kept after a checkpoint resets it.
const WRITER_PRAGMAS: &str = "PRAGMA synchronous=OFF; PRAGMA auto_vacuum=INCREMENTAL;
     PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
     PRAGMA cache_size=-8192; PRAGMA foreign_keys=ON; PRAGMA journal_size_limit=67108864;";
const AUTO_VACUUM_INCREMENTAL: i64 = 2;

/// Opens and initializes the writer connection. A corrupt index is quarantined
/// and recreated once when no other writer holds the lease.
pub(super) fn open(
    cache: &Path,
    root: &Path,
    deadline: Option<&QueryDeadline>,
) -> Result<Connection> {
    match open_initialized(cache, root, deadline) {
        Err(error) if is_index_corrupt(&error) && quarantine_unless_leased(cache)? => {
            open_initialized(cache, root, deadline)
        }
        opened => opened,
    }
}

/// Opens and initializes the writer connection without corruption recovery.
pub(super) fn open_initialized(
    cache: &Path,
    root: &Path,
    deadline: Option<&QueryDeadline>,
) -> Result<Connection> {
    let conn = Connection::open(cache.join(INDEX_NAME))
        .map_err(|error| cache_access::index_cache_unavailable(cache, error.into()))?;
    let initialized = initialize(&conn, cache, root, deadline);
    let initialized = match deadline {
        Some(_) => match (initialized, index_deadline::clear(&conn)) {
            (Err(primary), _) => Err(primary),
            (Ok(()), Err(cleanup)) => Err(cleanup),
            (Ok(()), Ok(())) => Ok(()),
        },
        None => initialized,
    };
    initialized.map_err(|error| {
        let error = cache_access::index_cache_unavailable(cache, error);
        match deadline {
            Some(deadline) => index_deadline::classify(deadline, error),
            None => error,
        }
    })?;
    Ok(conn)
}

fn initialize(
    conn: &Connection,
    cache: &Path,
    root: &Path,
    deadline: Option<&QueryDeadline>,
) -> Result<()> {
    match deadline {
        Some(deadline) => index_deadline::install(conn, deadline)?,
        None => conn.busy_timeout(index_deadline::SQLITE_DEFAULT_BUSY_TIMEOUT)?,
    }
    conn.execute_batch(WRITER_PRAGMAS)?;
    ensure_schema_revision(conn, cache)?;
    conn.execute(
        "INSERT OR IGNORE INTO meta(key,value) VALUES('index_epoch',?1)",
        [uuid::Uuid::new_v4().to_string()],
    )?;
    let stored_root: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key='root'", [], |row| {
            row.get(0)
        })
        .optional()?;
    let encoded_root = encode_path(root);
    anyhow::ensure!(
        stored_root
            .as_ref()
            .is_none_or(|stored| stored == &encoded_root),
        "cache belongs to another repository root"
    );
    conn.execute(
        "INSERT OR IGNORE INTO meta(key,value) VALUES('root',?1)",
        [&encoded_root],
    )?;
    if let Some(deadline) = deadline {
        index_deadline::check(deadline)?;
    }
    Ok(())
}

/// Creates the schema in an empty database. A database stamped with another
/// revision is emptied first, only while the writer lease is free so no pass
/// of another writer loses its tables; concurrent openers re-check under lock.
fn ensure_schema_revision(conn: &Connection, cache: &Path) -> Result<()> {
    if stored_revision(conn)? == schema::INDEX_SCHEMA_REVISION {
        return Ok(());
    }
    if !table_names(conn, false)?.is_empty() {
        let Some(_lease) = try_writer_lease(cache)? else {
            anyhow::bail!(
                "index_warming: index schema revision differs and another writer holds the lease"
            );
        };
        discard_tables(conn)?;
    }
    enable_incremental_vacuum(conn)?;
    let transaction = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    if stored_revision(&transaction)? != schema::INDEX_SCHEMA_REVISION {
        create_fresh(&transaction)?;
    }
    transaction.commit()?;
    Ok(())
}

/// Creates every table and stamps the revision; the caller opens the transaction.
pub(super) fn create_fresh(conn: &Connection) -> Result<()> {
    schema::create(conn)?;
    publish::create_journal(conn)?;
    conn.pragma_update(None, "user_version", schema::INDEX_SCHEMA_REVISION)?;
    Ok(())
}

fn stored_revision(conn: &Connection) -> Result<i64> {
    Ok(conn.pragma_query_value(None, "user_version", |row| row.get(0))?)
}

/// Drops every table of a database whose revision differs. Foreign keys are
/// off so dropping parent tables skips the implicit per-row delete.
fn discard_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
    let discarded = (|| -> Result<()> {
        conn.execute_batch("BEGIN IMMEDIATE;")?;
        let dropped = (|| -> Result<()> {
            if stored_revision(conn)? == schema::INDEX_SCHEMA_REVISION {
                return Ok(());
            }
            // Virtual tables first: dropping one removes its shadow tables.
            for virtual_only in [true, false] {
                for name in table_names(conn, virtual_only)? {
                    conn.execute_batch(&format!(
                        "DROP TABLE IF EXISTS \"{}\";",
                        name.replace('"', "\"\"")
                    ))?;
                }
            }
            conn.pragma_update(None, "user_version", 0)?;
            Ok(())
        })();
        match dropped {
            Ok(()) => conn.execute_batch("COMMIT;")?,
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK;");
                return Err(error);
            }
        }
        Ok(())
    })();
    let restored = conn.execute_batch("PRAGMA foreign_keys=ON;");
    discarded?;
    Ok(restored?)
}

fn table_names(conn: &Connection, virtual_only: bool) -> Result<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'
           AND (?1=0 OR sql LIKE 'CREATE VIRTUAL TABLE%') ORDER BY name",
    )?;
    let names = statement
        .query_map([virtual_only], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(names)
}

/// Ensures incremental auto-vacuum before tables exist; a file that held tables
/// needs one VACUUM, which is cheap once its tables are dropped.
fn enable_incremental_vacuum(conn: &Connection) -> Result<()> {
    let mode: i64 = conn.pragma_query_value(None, "auto_vacuum", |row| row.get(0))?;
    if mode == AUTO_VACUUM_INCREMENTAL {
        return Ok(());
    }
    conn.execute_batch("PRAGMA auto_vacuum=INCREMENTAL;")?;
    let mode: i64 = conn.pragma_query_value(None, "auto_vacuum", |row| row.get(0))?;
    if mode != AUTO_VACUUM_INCREMENTAL {
        conn.execute_batch("VACUUM;")?;
    }
    Ok(())
}

/// Whether SQLite reported the database file corrupt or not a database.
pub(super) fn is_index_corrupt(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<SqliteError>().is_some_and(|error| {
            matches!(
                error,
                SqliteError::SqliteFailure(failure, _)
                    if matches!(failure.code, ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase)
            )
        })
    })
}

/// Quarantines the index when the writer lease is free; false when another writer holds it.
fn quarantine_unless_leased(cache: &Path) -> Result<bool> {
    match try_writer_lease(cache)? {
        Some(_lease) => quarantine(cache).map(|()| true),
        None => Ok(false),
    }
}

/// The writer lease (`index.lock`) when no other writer holds it.
fn try_writer_lease(cache: &Path) -> Result<Option<std::fs::File>> {
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join("index.lock"))
        .context("open index writer lease")?;
    match fs2::FileExt::try_lock_exclusive(&lock) {
        Ok(()) => Ok(Some(lock)),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error).context("lock index writer lease"),
    }
}

/// Moves the index and its WAL aside as the single retained quarantine and
/// removes the shared-memory file. The caller holds the writer lease.
pub(super) fn quarantine(cache: &Path) -> Result<()> {
    for suffix in ["", "-wal"] {
        let from = cache.join(format!("{INDEX_NAME}{suffix}"));
        let to = cache.join(format!("{QUARANTINE_NAME}{suffix}"));
        match std::fs::rename(&from, &to) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                remove_if_present(&to)?;
            }
            Err(error) => return Err(error).context("quarantine corrupt index"),
        }
    }
    remove_if_present(&cache.join(format!("{INDEX_NAME}-shm")))
}

fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

/// Whether an integrity check finds the open index damaged.
pub(super) fn fails_quick_check(conn: &Connection) -> bool {
    conn.query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0))
        .map_or(true, |result| result != "ok")
}

/// Returns freed pages to the filesystem and truncates the WAL after a
/// publication. Best effort: a reader on an older snapshot leaves the WAL in
/// place, and the checkpoint never waits on its busy handler.
pub(super) fn reclaim_after_publish(
    conn: &Connection,
    deadline: Option<&QueryDeadline>,
) -> Result<()> {
    if deadline.is_some_and(QueryDeadline::expired) {
        return Ok(());
    }
    // Each step frees pages, so the statement runs to completion.
    let mut vacuum = conn.prepare("PRAGMA incremental_vacuum")?;
    let mut steps = vacuum.query([])?;
    while steps.next()?.is_some() {}
    drop(steps);
    drop(vacuum);
    conn.busy_timeout(Duration::ZERO)?;
    let checkpoint = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        row.get::<_, i64>(0)
    });
    let busy = match deadline {
        Some(deadline) => deadline.cap(index_deadline::SQLITE_BUSY_TIMEOUT),
        None => index_deadline::SQLITE_DEFAULT_BUSY_TIMEOUT,
    };
    conn.busy_timeout(busy)?;
    checkpoint?;
    Ok(())
}

#[cfg(test)]
mod tests;
