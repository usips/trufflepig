//! Query-only access: no DDL, no writes to `index.sqlite3`, bounded waits, and an
//! optional worktree view that reads its own bytes through a parent's index.
use super::{Store, StoreAccess, encode_path};
use crate::daemon::deadline::QueryDeadline;
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::{path::Path, time::Duration};

/// Longest a reader waits on a SQLite lock (capped by its query deadline).
const READ_BUSY_TIMEOUT: Duration = Duration::from_secs(2);
/// SQLite virtual-machine steps between deadline checks.
const DEADLINE_CHECK_STEPS: i32 = 4_096;
const INDEX_WARMING: &str = "index_warming: initial index is not published";

impl Store {
    /// Opens a published index read-only for one query. A missing database or
    /// generation 0 is `index_warming`; the SQLite progress handler interrupts
    /// statements once `deadline` expires (see [`QueryDeadline::classify`]).
    pub fn open_read(index_root: &Path, cache: &Path, deadline: QueryDeadline) -> Result<Self> {
        let root = index_root
            .canonicalize()
            .context("canonicalize repository root")?;
        ensure!(root.is_dir(), "repository root is not a directory");
        let Ok(cache) = cache.canonicalize() else {
            bail!(INDEX_WARMING);
        };
        let database = cache.join("index.sqlite3");
        ensure!(database.is_file(), INDEX_WARMING);
        let conn = Connection::open_with_flags(
            &database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(deadline.cap(READ_BUSY_TIMEOUT))?;
        conn.execute_batch("PRAGMA query_only=ON; PRAGMA cache_size=-2048;")?;
        conn.progress_handler(DEADLINE_CHECK_STEPS, Some(move || deadline.expired()))?;
        let (stored_root, generation) =
            published_state(&conn).map_err(|error| deadline.classify(error))?;
        ensure!(
            stored_root == encode_path(&root),
            "cache belongs to another repository root"
        );
        ensure!(generation > 0, INDEX_WARMING);
        Ok(Self {
            conn,
            index_root: root.clone(),
            root,
            results_cache: cache.clone(),
            cache,
            result_sets: std::cell::OnceCell::new(),
            access: StoreAccess::Reader,
        })
    }

    /// Reads current bytes from `read_root` (a linked worktree) through this
    /// index; stored paths are root-relative, so they address the same files.
    /// Result sets are saved in `results_cache`, the read root's own cache.
    pub fn reading_from(mut self, read_root: &Path, results_cache: &Path) -> Result<Self> {
        ensure!(
            self.access == StoreAccess::Reader,
            "read_only_store: only a query-only store reads another root"
        );
        let read_root = read_root.canonicalize().context("canonicalize read root")?;
        ensure!(read_root.is_dir(), "read root is not a directory");
        self.root = read_root;
        self.results_cache = results_cache.to_path_buf();
        self.result_sets = std::cell::OnceCell::new();
        Ok(self)
    }
}

/// Whether `error` is [`Store::open_read`]'s unpublished-index answer.
pub fn is_index_warming(error: &anyhow::Error) -> bool {
    error.to_string().starts_with("index_warming:")
}

/// The recorded root and generation; an index whose schema is still being
/// created reads as warming.
fn published_state(conn: &Connection) -> Result<(String, i64)> {
    let has_meta: bool = conn.query_row(
        "SELECT count(*)>0 FROM sqlite_master WHERE type='table' AND name='meta'",
        [],
        |row| row.get(0),
    )?;
    ensure!(has_meta, INDEX_WARMING);
    let read = |key: &str| -> Result<Option<String>> {
        Ok(conn
            .query_row("SELECT value FROM meta WHERE key=?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    };
    let root = read("root")?.context(INDEX_WARMING)?;
    let generation = read("generation")?.context(INDEX_WARMING)?.parse()?;
    Ok((root, generation))
}
