use super::{PreparationState, PreparationStatus};
use crate::semantic::Embedding;
#[cfg(feature = "semantic")]
use crate::semantic::embedding_cache::EmbeddingCache;
#[cfg(feature = "semantic")]
use crate::semantic::embedding_cache::EmbeddingCacheSnapshot;
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    fs::{self, File, OpenOptions},
    io::ErrorKind,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const RUN_RETENTION: i64 = 256;

#[derive(Clone, Debug)]
pub(super) struct RunRecord {
    pub state: PreparationState,
    pub cursor: usize,
    pub total: usize,
    pub cached: usize,
    pub missing: usize,
    pub failures: usize,
    pub error: Option<String>,
}

pub(super) struct PreparationCache {
    pub(super) db: Connection,
    #[cfg(feature = "semantic")]
    embedding_cache: EmbeddingCache,
}

pub(super) enum CachedSnapshot {
    #[cfg(feature = "semantic")]
    Semantic(EmbeddingCacheSnapshot),
    #[cfg(not(feature = "semantic"))]
    Completions,
}

impl PreparationCache {
    pub(super) fn open(directory: &Path) -> Result<Self> {
        fs::create_dir_all(directory).context("create preparation cache directory")?;
        let db = Connection::open(directory.join("preparation.sqlite3"))?;
        db.busy_timeout(std::time::Duration::from_secs(10))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             CREATE TABLE IF NOT EXISTS preparation_requests(
                 id INTEGER PRIMARY KEY CHECK(id=1),
                 requested_generation INTEGER NOT NULL);
             INSERT OR IGNORE INTO preparation_requests VALUES(1,0);
             CREATE TABLE IF NOT EXISTS preparation_runs(
                 generation INTEGER PRIMARY KEY,
                 state TEXT NOT NULL,
                 cursor INTEGER NOT NULL,
                 total INTEGER NOT NULL,
                 cached INTEGER NOT NULL,
                 missing INTEGER NOT NULL,
                 failures INTEGER NOT NULL,
                 error TEXT,
                 updated_ms INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS preparation_completions(
                 content_key TEXT PRIMARY KEY,
                 outcome TEXT NOT NULL CHECK(outcome IN ('complete','failed')),
                 error TEXT,
                 completed_ms INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS preparation_runs_updated
                 ON preparation_runs(updated_ms,generation);",
        )?;
        #[cfg(feature = "semantic")]
        let embedding_cache = EmbeddingCache::open(directory)?;
        Ok(Self {
            db,
            #[cfg(feature = "semantic")]
            embedding_cache,
        })
    }

    pub(super) fn requested_generation(&self) -> Result<i64> {
        Ok(self.db.query_row(
            "SELECT requested_generation FROM preparation_requests WHERE id=1",
            [],
            |row| row.get(0),
        )?)
    }

    pub(super) fn request(&mut self, generation: i64) -> Result<bool> {
        if generation <= 0 {
            bail!("preparation_unavailable: no published source generation");
        }
        let previous = self.requested_generation()?;
        self.db.execute(
            "UPDATE preparation_requests SET requested_generation=max(requested_generation,?1) WHERE id=1",
            [generation],
        )?;
        Ok(previous >= generation)
    }

    pub(super) fn run(&self, generation: i64) -> Result<Option<RunRecord>> {
        self.db
            .query_row(
                "SELECT generation,state,cursor,total,cached,missing,failures,error
                 FROM preparation_runs WHERE generation=?1",
                [generation],
                |row| {
                    Ok(RunRecord {
                        state: row.get::<_, String>(1)?.parse().map_err(|error: String| {
                            rusqlite::Error::FromSqlConversionFailure(
                                1,
                                rusqlite::types::Type::Text,
                                Box::new(std::io::Error::new(ErrorKind::InvalidData, error)),
                            )
                        })?,
                        cursor: row.get::<_, i64>(2)?.try_into().map_err(|_| {
                            rusqlite::Error::FromSqlConversionFailure(
                                2,
                                rusqlite::types::Type::Integer,
                                Box::new(std::io::Error::new(
                                    ErrorKind::InvalidData,
                                    "negative preparation cursor",
                                )),
                            )
                        })?,
                        total: row.get::<_, i64>(3)?.try_into().map_err(|_| {
                            rusqlite::Error::FromSqlConversionFailure(
                                3,
                                rusqlite::types::Type::Integer,
                                Box::new(std::io::Error::new(
                                    ErrorKind::InvalidData,
                                    "invalid preparation total",
                                )),
                            )
                        })?,
                        cached: row.get::<_, i64>(4)?.try_into().map_err(|_| {
                            rusqlite::Error::FromSqlConversionFailure(
                                4,
                                rusqlite::types::Type::Integer,
                                Box::new(std::io::Error::new(
                                    ErrorKind::InvalidData,
                                    "invalid cached count",
                                )),
                            )
                        })?,
                        missing: row.get::<_, i64>(5)?.try_into().map_err(|_| {
                            rusqlite::Error::FromSqlConversionFailure(
                                5,
                                rusqlite::types::Type::Integer,
                                Box::new(std::io::Error::new(
                                    ErrorKind::InvalidData,
                                    "invalid missing count",
                                )),
                            )
                        })?,
                        failures: row.get::<_, i64>(6)?.try_into().map_err(|_| {
                            rusqlite::Error::FromSqlConversionFailure(
                                6,
                                rusqlite::types::Type::Integer,
                                Box::new(std::io::Error::new(
                                    ErrorKind::InvalidData,
                                    "invalid failure count",
                                )),
                            )
                        })?,
                        error: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub(super) fn begin(
        &mut self,
        generation: i64,
        total: usize,
        reset_failed: bool,
    ) -> Result<RunRecord> {
        let existing = self.run(generation)?;
        if let Some(existing) = existing {
            if reset_failed && existing.state == PreparationState::Failed {
                self.reset_failed_run(generation, Some(total))?;
                return self
                    .run(generation)
                    .and_then(|record| record.context("preparation run was not persisted"));
            }
            return Ok(existing);
        }
        let now = timestamp_ms();
        self.db.execute(
            "INSERT INTO preparation_runs
             (generation,state,cursor,total,cached,missing,failures,error,updated_ms)
             VALUES(?1,'running',0,?2,0,0,0,NULL,?3)
             ON CONFLICT(generation) DO UPDATE SET
               state='running',cursor=0,total=excluded.total,cached=0,
               missing=0,failures=0,error=NULL,updated_ms=excluded.updated_ms",
            params![generation, total as i64, now],
        )?;
        self.prune_runs(generation)?;
        self.run(generation)
            .and_then(|record| record.context("preparation run was not persisted"))
    }

    /// Reopens a failed run for an explicit preparation request.
    pub(super) fn reset_failed(&mut self, generation: i64) -> Result<bool> {
        self.reset_failed_run(generation, None)
    }

    fn reset_failed_run(&mut self, generation: i64, total: Option<usize>) -> Result<bool> {
        let tx = self.db.transaction()?;
        let changed = tx.execute(
            "UPDATE preparation_runs SET state='running',cursor=0,cached=0,
             total=coalesce(?2,total),missing=0,failures=0,error=NULL,updated_ms=?3
             WHERE generation=?1 AND state='failed'",
            params![generation, total.map(|total| total as i64), timestamp_ms()],
        )?;
        if changed != 0 {
            tx.execute(
                "DELETE FROM preparation_completions WHERE outcome='failed'",
                [],
            )?;
        }
        tx.commit()?;
        Ok(changed != 0)
    }

    pub(super) fn progress(
        &mut self,
        generation: i64,
        cursor: usize,
        total: usize,
        cached: usize,
        missing: usize,
        failures: usize,
    ) -> Result<()> {
        self.db.execute(
            "UPDATE preparation_runs SET state='running',cursor=?2,total=?3,cached=?4,
             missing=?5,failures=?6,error=NULL,updated_ms=?7 WHERE generation=?1",
            params![
                generation,
                cursor as i64,
                total as i64,
                cached as i64,
                missing as i64,
                failures as i64,
                timestamp_ms(),
            ],
        )?;
        Ok(())
    }

    pub(super) fn finish(
        &mut self,
        generation: i64,
        state: PreparationState,
        cursor: usize,
        total: usize,
        cached: usize,
        missing: usize,
        failures: usize,
        error: Option<&str>,
    ) -> Result<()> {
        self.db.execute(
            "UPDATE preparation_runs SET state=?2,cursor=?3,total=?4,cached=?5,
             missing=?6,failures=?7,error=?8,updated_ms=?9 WHERE generation=?1",
            params![
                generation,
                state.as_str(),
                cursor as i64,
                total as i64,
                cached as i64,
                missing as i64,
                failures as i64,
                error,
                timestamp_ms(),
            ],
        )?;
        Ok(())
    }

    pub(super) fn supersede_older(&mut self, generation: i64) -> Result<()> {
        self.db.execute(
            "UPDATE preparation_runs SET state='superseded',
             error=coalesce(error,'source generation superseded'),updated_ms=?1
             WHERE generation<?2 AND state='running'",
            params![timestamp_ms(), generation],
        )?;
        Ok(())
    }

    pub(super) fn supersede_generation(
        &mut self,
        generation: i64,
        total: usize,
        error: &str,
    ) -> Result<()> {
        self.db.execute(
            "INSERT INTO preparation_runs
             (generation,state,cursor,total,cached,missing,failures,error,updated_ms)
             VALUES(?1,'superseded',0,?2,0,0,0,?3,?4)
             ON CONFLICT(generation) DO UPDATE SET
               state=CASE WHEN preparation_runs.state='running' THEN 'superseded'
                          ELSE preparation_runs.state END,
               error=CASE WHEN preparation_runs.state='running'
                          THEN excluded.error ELSE preparation_runs.error END,
               updated_ms=excluded.updated_ms",
            params![generation, total as i64, error, timestamp_ms()],
        )?;
        Ok(())
    }

    pub(super) fn complete_many(
        &mut self,
        entries: impl IntoIterator<Item = (String, Embedding)>,
    ) -> Result<()> {
        let entries = entries.into_iter().collect::<Vec<_>>();
        if entries.is_empty() {
            return Ok(());
        }
        #[cfg(feature = "semantic")]
        self.embedding_cache
            .put_many(entries.iter().map(|(key, vector)| (key, vector)))?;
        let tx = self.db.transaction()?;
        for (key, _) in entries {
            tx.execute(
                "INSERT OR IGNORE INTO preparation_completions
                 (content_key,outcome,error,completed_ms) VALUES(?1,'complete',NULL,?2)",
                params![key, timestamp_ms()],
            )?;
        }
        Ok(tx.commit()?)
    }

    pub(super) fn fail(&mut self, key: &str, error: &str) -> Result<()> {
        self.db.execute(
            "INSERT OR IGNORE INTO preparation_completions
             (content_key,outcome,error,completed_ms) VALUES(?1,'failed',?2,?3)",
            params![key, error, timestamp_ms()],
        )?;
        Ok(())
    }

    /// Removes completion metadata for content absent from the current source.
    pub(super) fn prune_completions<I, S>(&mut self, current_keys: I) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let tx = self.db.transaction()?;
        tx.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS preparation_current_content(
                 content_key TEXT PRIMARY KEY);
             DELETE FROM preparation_current_content;",
        )?;
        for key in current_keys {
            tx.execute(
                "INSERT OR IGNORE INTO preparation_current_content(content_key) VALUES(?1)",
                [key.as_ref()],
            )?;
        }
        tx.execute(
            "DELETE FROM preparation_completions
             WHERE content_key NOT IN
               (SELECT content_key FROM preparation_current_content)",
            [],
        )?;
        Ok(tx.commit()?)
    }

    pub(super) fn completion(&self, key: &str) -> Result<Option<bool>> {
        self.db
            .query_row(
                "SELECT outcome='complete' FROM preparation_completions WHERE content_key=?1",
                [key],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub(super) fn snapshot(&self) -> Result<CachedSnapshot> {
        #[cfg(feature = "semantic")]
        {
            return Ok(CachedSnapshot::Semantic(
                self.embedding_cache.begin_snapshot()?,
            ));
        }
        #[cfg(not(feature = "semantic"))]
        Ok(CachedSnapshot::Completions)
    }

    /// A successful preparation is cached only while its verified vector exists.
    pub(super) fn is_cached_in(&self, key: &str, snapshot: &CachedSnapshot) -> Result<bool> {
        #[cfg(feature = "semantic")]
        {
            let CachedSnapshot::Semantic(snapshot) = snapshot;
            return snapshot.get(key).map(|vector| vector.is_some());
        }
        #[cfg(not(feature = "semantic"))]
        let _ = snapshot;
        #[cfg(not(feature = "semantic"))]
        Ok(self.completion(key)?.is_some_and(|success| success))
    }

    pub(super) fn counts_for_status(
        &self,
        generation: i64,
        total: usize,
        cached: usize,
        missing: usize,
        failures: usize,
    ) -> Result<PreparationStatus> {
        let run = self.run(generation)?;
        let (state, cursor, run_failures, error) = run
            .as_ref()
            .map(|run| (run.state, run.cursor, run.failures, run.error.clone()))
            .unwrap_or((PreparationState::Idle, 0, 0, None));
        let (state, error) = if state == PreparationState::Completed && missing > 0 {
            (
                PreparationState::Capacity,
                error.or_else(|| Some("embedding cache capacity cannot retain all content".into())),
            )
        } else {
            (state, error)
        };
        Ok(PreparationStatus {
            generation,
            state,
            cursor,
            total,
            cached,
            missing,
            failures: failures.max(run_failures),
            error,
        })
    }

    fn prune_runs(&mut self, _newest: i64) -> Result<()> {
        self.db.execute(
            "DELETE FROM preparation_runs
             WHERE generation NOT IN
               (SELECT generation FROM preparation_runs ORDER BY generation DESC LIMIT ?1)",
            [RUN_RETENTION],
        )?;
        Ok(())
    }

    /// Persists a worker failure even when the sweep failed before `begin`.
    pub(super) fn record_worker_error(&mut self, generation: i64, message: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO preparation_runs
             (generation,state,cursor,total,cached,missing,failures,error,updated_ms)
             VALUES(?1,'failed',0,0,0,0,1,?2,?3)
             ON CONFLICT(generation) DO UPDATE SET
               state='failed',failures=max(preparation_runs.failures,1),
               error=?2,updated_ms=?3",
            params![generation, message, timestamp_ms()],
        )?;
        Ok(())
    }
}

/// Cross-process lease for the one background sweep allowed for a root.
pub(super) struct PreparationLease {
    file: File,
    _path: PathBuf,
}

impl PreparationLease {
    pub(super) fn try_acquire(directory: &Path) -> Result<Option<Self>> {
        fs::create_dir_all(directory)?;
        let directory = fs::canonicalize(directory)?;
        let path = directory.join("preparation.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(Self { file, _path: path })),
            Err(error) if error.kind() == ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error).context("acquire preparation lease"),
        }
    }
}

impl Drop for PreparationLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub(super) fn timestamp_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}
