//! Layout of `results.sqlite3`. The file is a cache, so an opener that finds a
//! different `user_version` drops the stale tables and rebuilds the file with
//! `VACUUM` instead of migrating. Incremental auto-vacuum lets eviction return
//! freed pages to the filesystem.

use super::{commit, execute_sql};
use crate::daemon::deadline::QueryDeadline;
use anyhow::Result;
use rusqlite::{Connection, Transaction, TransactionBehavior};

/// Bump on any change to the `result_sets` layout.
pub(super) const RESULT_SCHEMA_VERSION: i64 = 1;
/// `PRAGMA auto_vacuum` value for incremental mode.
const INCREMENTAL_AUTO_VACUUM: i64 = 2;

/// `bytes` precedes `payload` so byte accounting never reads payload overflow pages.
const CREATE_RESULT_SETS: &str = "CREATE TABLE result_sets(
    id TEXT PRIMARY KEY,
    created INTEGER NOT NULL,
    expires INTEGER NOT NULL,
    bytes INTEGER NOT NULL,
    payload TEXT NOT NULL
)";

/// Applies per-connection settings; a current file needs nothing else.
pub(super) fn prepare(conn: &Connection, deadline: Option<&QueryDeadline>) -> Result<()> {
    execute_sql(conn, deadline, || {
        conn.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA journal_size_limit=4194304;")
    })?;
    if schema_version(conn, deadline)? == RESULT_SCHEMA_VERSION {
        return Ok(());
    }
    rebuild(conn, deadline)
}

fn schema_version(conn: &Connection, deadline: Option<&QueryDeadline>) -> Result<i64> {
    pragma(conn, deadline, "user_version")
}

/// Each step rechecks the version under the write lock, so concurrent openers
/// never drop a layout another opener just stamped. A new file is created in
/// one transaction; only a stale file pays for `VACUUM`.
fn rebuild(conn: &Connection, deadline: Option<&QueryDeadline>) -> Result<()> {
    execute_sql(conn, deadline, || {
        conn.execute_batch("PRAGMA auto_vacuum=INCREMENTAL; PRAGMA journal_mode=WAL;")
    })?;
    let current = in_write_transaction(conn, deadline, |tx| {
        if schema_version(tx, deadline)? == RESULT_SCHEMA_VERSION {
            return Ok(true);
        }
        execute_sql(tx, deadline, || {
            tx.execute_batch("DROP TABLE IF EXISTS result_sets")
        })?;
        let compact = pragma(tx, deadline, "auto_vacuum")? == INCREMENTAL_AUTO_VACUUM
            && pragma(tx, deadline, "freelist_count")? == 0;
        if compact {
            create(tx, deadline)?;
        }
        Ok(compact)
    })?;
    if current {
        return Ok(());
    }
    // Rewrites the file, applying incremental auto-vacuum and discarding old free pages.
    execute_sql(conn, deadline, || conn.execute_batch("VACUUM"))?;
    in_write_transaction(conn, deadline, |tx| {
        if schema_version(tx, deadline)? == RESULT_SCHEMA_VERSION {
            return Ok(());
        }
        create(tx, deadline)
    })
}

fn create(tx: &Transaction, deadline: Option<&QueryDeadline>) -> Result<()> {
    execute_sql(tx, deadline, || {
        tx.execute_batch(&format!(
            "{CREATE_RESULT_SETS}; PRAGMA user_version={RESULT_SCHEMA_VERSION};"
        ))
    })
}

fn pragma(conn: &Connection, deadline: Option<&QueryDeadline>, name: &str) -> Result<i64> {
    execute_sql(conn, deadline, || {
        conn.query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
    })
}

fn in_write_transaction<T>(
    conn: &Connection,
    deadline: Option<&QueryDeadline>,
    work: impl FnOnce(&Transaction) -> Result<T>,
) -> Result<T> {
    let tx = execute_sql(conn, deadline, || {
        Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
    })?;
    let value = work(&tx)?;
    commit(conn, deadline, tx)?;
    Ok(value)
}
