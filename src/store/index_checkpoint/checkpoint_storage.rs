use crate::daemon::deadline::QueryDeadline;
use crate::extract::Extraction;
use crate::store::index_deadline;
use anyhow::{Context, Result};
use rusqlite::{Connection, Error as SqliteError, ErrorCode, OptionalExtension, params};
use std::path::Path;
use std::time::{Duration, Instant};

/// A bounded slice commits its open batch this often or after this many files.
const COMMIT_INTERVAL: Duration = Duration::from_millis(250);
const COMMIT_FILE_BATCH: usize = 256;

/// The retained preparation database. Writes join one open transaction that is
/// committed by [`Self::commit_if_due`] or [`Self::commit`]; the checkpoint is a
/// disposable cache, so `synchronous=NORMAL` is sufficient.
pub(super) struct CheckpointStorage {
    conn: Connection,
    /// Bounded slices commit on a cadence; unbounded passes commit once per pass.
    deadline: Option<QueryDeadline>,
    transaction_started: Option<Instant>,
    files_in_transaction: usize,
    #[cfg(test)]
    fail_next_mark_seen: std::cell::Cell<bool>,
    #[cfg(test)]
    commits: usize,
}

impl CheckpointStorage {
    pub(super) fn open(path: &Path, deadline: Option<&QueryDeadline>) -> Result<Self> {
        let conn = Connection::open(path).context("open retained index preparation")?;
        if let Some(deadline) = deadline {
            index_deadline::install(&conn, deadline)?;
        } else {
            conn.busy_timeout(index_deadline::SQLITE_DEFAULT_BUSY_TIMEOUT)?;
        }
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS checkpoint_meta(
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS completed_files(
                 path TEXT PRIMARY KEY,
                 revision TEXT,
                 source_status TEXT NOT NULL,
                 bytes INTEGER NOT NULL,
                 extraction_version INTEGER NOT NULL,
                 facts TEXT NOT NULL
             );
             CREATE TEMP TABLE IF NOT EXISTS scan_seen(path TEXT PRIMARY KEY);",
        )?;
        let completed_files = conn
            .prepare(
                "SELECT path, revision, source_status, bytes, extraction_version, facts
                 FROM completed_files LIMIT 0",
            )
            .context("validate retained completed-file schema")?;
        drop(completed_files);
        Ok(Self {
            conn,
            deadline: deadline.cloned(),
            transaction_started: None,
            files_in_transaction: 0,
            #[cfg(test)]
            fail_next_mark_seen: std::cell::Cell::new(false),
            #[cfg(test)]
            commits: 0,
        })
    }

    pub(super) fn is_bounded(&self) -> bool {
        self.deadline.is_some()
    }

    /// Opens the batch transaction that the next write joins, if none is open.
    fn begin(&mut self) -> Result<()> {
        if self.conn.is_autocommit() {
            self.conn.execute_batch("BEGIN")?;
            self.transaction_started = Some(Instant::now());
        }
        Ok(())
    }

    /// Commits the open batch. A bounded deadline's progress handler is removed first
    /// so that expiry cannot abort the COMMIT, then reattached for the rest of the slice.
    pub(super) fn commit(&mut self) -> Result<()> {
        if !self.conn.is_autocommit() {
            if self.deadline.is_some() {
                index_deadline::clear(&self.conn)?;
            }
            self.conn.execute_batch("COMMIT")?;
            #[cfg(test)]
            {
                self.commits += 1;
            }
        }
        self.transaction_started = None;
        self.files_in_transaction = 0;
        if let Some(deadline) = &self.deadline {
            index_deadline::attach(&self.conn, deadline)?;
        }
        Ok(())
    }

    /// Commits a bounded batch that is old enough or large enough. Unbounded passes
    /// keep one transaction until [`Self::commit`] at the end of the pass.
    pub(super) fn commit_if_due(&mut self) -> Result<()> {
        let due = self.files_in_transaction >= COMMIT_FILE_BATCH
            || self
                .transaction_started
                .is_some_and(|started| started.elapsed() >= COMMIT_INTERVAL);
        if self.deadline.is_some() && due {
            self.commit()?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn commit_count(&self) -> usize {
        self.commits
    }

    pub(super) fn metadata(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM checkpoint_meta WHERE key=?1",
                [key],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub(super) fn set_metadata(&mut self, key: &str, value: &str) -> Result<()> {
        self.begin()?;
        self.conn.execute(
            "INSERT INTO checkpoint_meta(key,value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub(super) fn extraction(
        &self,
        path: &str,
        revision: Option<&str>,
        source_status: &str,
        bytes: u64,
        extraction_version: i64,
    ) -> Result<Option<Extraction>> {
        let facts: Option<String> = self
            .conn
            .query_row(
                "SELECT facts FROM completed_files
                 WHERE path=?1 AND revision IS ?2 AND source_status=?3
                   AND bytes=?4 AND extraction_version=?5",
                params![
                    path,
                    revision,
                    source_status,
                    bytes as i64,
                    extraction_version
                ],
                |row| row.get(0),
            )
            .optional()?;
        facts
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .context("decode retained extraction")
    }

    pub(super) fn save_extraction(
        &mut self,
        path: &str,
        revision: Option<&str>,
        source_status: &str,
        bytes: u64,
        extraction_version: i64,
        extraction: &Extraction,
    ) -> Result<bool> {
        let facts = serde_json::to_string(extraction).context("encode retained extraction")?;
        self.begin()?;
        let already_present: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM completed_files WHERE path=?1)",
            [path],
            |row| row.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO completed_files(path,revision,source_status,bytes,extraction_version,facts)
             VALUES(?1,?2,?3,?4,?5,?6)
             ON CONFLICT(path) DO UPDATE SET revision=excluded.revision,
                 source_status=excluded.source_status,bytes=excluded.bytes,
                 extraction_version=excluded.extraction_version,facts=excluded.facts",
            params![
                path,
                revision,
                source_status,
                bytes as i64,
                extraction_version,
                facts
            ],
        )?;
        Ok(!already_present)
    }

    pub(super) fn start_scan(&mut self) -> Result<()> {
        self.begin()?;
        self.conn.execute("DELETE FROM scan_seen", [])?;
        Ok(())
    }

    pub(super) fn mark_seen(&mut self, path: &str) -> Result<()> {
        #[cfg(test)]
        if self.fail_next_mark_seen.replace(false) {
            anyhow::bail!("injected checkpoint mark_seen failure");
        }
        self.begin()?;
        self.conn
            .execute("INSERT OR IGNORE INTO scan_seen(path) VALUES(?1)", [path])?;
        self.files_in_transaction += 1;
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn fail_next_mark_seen_for_test(&self) {
        self.fail_next_mark_seen.set(true);
    }

    pub(super) fn finish_scan(&mut self) -> Result<()> {
        self.begin()?;
        self.conn.execute(
            "DELETE FROM completed_files WHERE path NOT IN (SELECT path FROM scan_seen)",
            [],
        )?;
        self.conn.execute("DELETE FROM scan_seen", [])?;
        Ok(())
    }

    pub(super) fn completed_count(&self) -> Result<usize> {
        let count: i64 =
            self.conn
                .query_row("SELECT count(*) FROM completed_files", [], |row| row.get(0))?;
        Ok(count.try_into()?)
    }
}

pub(super) fn is_disposable_stage_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        let sqlite_corrupt = cause
            .downcast_ref::<SqliteError>()
            .is_some_and(|error| match error {
                SqliteError::SqliteFailure(failure, _) => matches!(
                    failure.code,
                    ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase
                ),
                _ => false,
            });
        let message = cause.to_string().to_ascii_lowercase();
        sqlite_corrupt
            || message.contains("no such table: checkpoint_meta")
            || message.contains("no such table: completed_files")
            || message.contains("no such column")
            || message.contains("malformed database schema")
    })
}
