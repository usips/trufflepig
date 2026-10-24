//! `<cache>/results.sqlite3`: persisted result sets, apart from the published
//! index so saving a query's results never waits behind a publication.
use super::{MAX_BYTES, MAX_HITS, StoredResultSet, now};
use crate::coded_error::CodedContext;
use crate::daemon::deadline::QueryDeadline;
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use std::{path::Path, thread, time::Duration};

mod result_set_schema;

const MAX_SETS: usize = 50;
const TTL_SECONDS: i64 = 600;
const MAINTENANCE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const SQL_BUSY_SLICE: Duration = Duration::from_millis(25);
const BUSY_RETRY_PAUSE: Duration = Duration::from_millis(3);
const SQL_CHECK_STEPS: i32 = 1_024;
/// Free pages one save returns to the filesystem; inserts reuse the rest.
const VACUUM_PAGES_PER_SAVE: u32 = 1_024;
pub const RESULTS_DATABASE: &str = "results.sqlite3";

/// One cache's result-set database; the only writer path a read query uses.
pub struct ResultSetStore {
    pub(crate) conn: Connection,
    deadline: Option<QueryDeadline>,
}

impl ResultSetStore {
    /// Opens the result database for explicit maintenance without a query budget.
    pub fn open(cache: &Path) -> Result<Self> {
        Self::open_inner(cache, None)
    }

    /// Opens the result database with the query's original absolute deadline.
    pub fn open_with_deadline(cache: &Path, deadline: &QueryDeadline) -> Result<Self> {
        ensure_live(Some(deadline))?;
        Self::open_inner(cache, Some(deadline.clone()))
    }

    fn open_inner(cache: &Path, deadline: Option<QueryDeadline>) -> Result<Self> {
        ensure_live(deadline.as_ref())?;
        std::fs::create_dir_all(cache).context("create result cache directory")?;
        ensure_live(deadline.as_ref())?;
        let conn =
            Connection::open(cache.join(RESULTS_DATABASE)).context("open result-set database")?;
        let store = Self { conn, deadline };
        match &store.deadline {
            Some(deadline) => {
                ensure_live(Some(deadline))?;
                let handler_deadline = deadline.clone();
                store
                    .conn
                    .progress_handler(SQL_CHECK_STEPS, Some(move || handler_deadline.expired()))?;
            }
            None => store.conn.busy_timeout(MAINTENANCE_BUSY_TIMEOUT)?,
        }
        result_set_schema::prepare(&store.conn, store.deadline.as_ref())?;
        Ok(store)
    }

    /// Assigns handles, bounds the set to [`MAX_HITS`] and [`MAX_BYTES`], and
    /// evicts the oldest other sets past the retention limits.
    pub fn save(&self, mut set: StoredResultSet) -> Result<String> {
        self.ensure_live()?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        if set.hits.len() > MAX_HITS {
            set.hits.truncate(MAX_HITS);
            set.truncated = true;
        }
        for (ordinal, hit) in set.hits.iter_mut().enumerate() {
            *hit.handle_mut() = format!("{id}:{}", ordinal + 1);
        }
        let mut payload = serde_json::to_string(&set)?;
        self.ensure_live()?;
        while payload.len() > MAX_BYTES && !set.hits.is_empty() {
            let keep = (set.hits.len() * MAX_BYTES / payload.len()).saturating_sub(1);
            set.hits.truncate(keep);
            set.truncated = true;
            payload = serde_json::to_string(&set)?;
            self.ensure_live()?;
        }
        if payload.len() > MAX_BYTES {
            bail!(coded!(
                ResultCacheUnavailable,
                "metadata exceeds cache capacity"
            ));
        }
        self.ensure_live()?;
        let tx = execute_sql(&self.conn, self.deadline.as_ref(), || {
            rusqlite::Transaction::new_unchecked(
                &self.conn,
                rusqlite::TransactionBehavior::Immediate,
            )
        })?;
        let time = now();
        let expired = execute_sql(&self.conn, self.deadline.as_ref(), || {
            tx.execute("DELETE FROM result_sets WHERE expires<=?1", [time])
        })?;
        execute_sql(&self.conn, self.deadline.as_ref(), || {
            tx.execute(
                "INSERT INTO result_sets(id,created,expires,bytes,payload) VALUES(?1,?2,?3,?4,?5)",
                params![id, time, time + TTL_SECONDS, payload.len() as i64, payload],
            )
        })?;
        let (mut count, mut bytes): (usize, usize) =
            execute_sql(&self.conn, self.deadline.as_ref(), || {
                tx.query_row(
                    "SELECT count(*),coalesce(sum(bytes),0) FROM result_sets",
                    [],
                    |r| Ok((r.get::<_, i64>(0)? as usize, r.get::<_, i64>(1)? as usize)),
                )
            })?;
        let mut evicted = 0;
        while count > MAX_SETS || bytes > MAX_BYTES {
            let oldest: Option<(String, i64)> = execute_sql(
                &self.conn,
                self.deadline.as_ref(),
                || {
                    tx.query_row(
                        "SELECT id,bytes FROM result_sets WHERE id != ?1 ORDER BY created,id LIMIT 1",
                        [&id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()
                },
            )?;
            let Some((oldest, oldest_bytes)) = oldest else {
                break;
            };
            execute_sql(&self.conn, self.deadline.as_ref(), || {
                tx.execute("DELETE FROM result_sets WHERE id=?1", [&oldest])
            })?;
            count -= 1;
            bytes = bytes.saturating_sub(oldest_bytes as usize);
            evicted += 1;
        }
        commit(&self.conn, self.deadline.as_ref(), tx)?;
        if expired + evicted > 0 {
            self.return_free_pages();
        }
        Ok(id)
    }

    /// Best effort after commit: a busy or expiring vacuum leaves pages for inserts to reuse.
    fn return_free_pages(&self) {
        let _ = self.conn.busy_timeout(busy_slice(self.deadline.as_ref()));
        // Each step frees one page, so the pragma runs to completion.
        let _ = (|| -> rusqlite::Result<()> {
            let mut vacuum = self.conn.prepare(&format!(
                "PRAGMA incremental_vacuum({VACUUM_PAGES_PER_SAVE})"
            ))?;
            let mut steps = vacuum.query([])?;
            while steps.next()?.is_some() {}
            Ok(())
        })();
    }

    pub fn load(&self, id: &str) -> Result<StoredResultSet> {
        self.ensure_live()?;
        uuid::Uuid::parse_str(id).with_coded_context(|| {
            coded!(InvalidHandle, "expected immutable result-set identifier")
        })?;
        let row: Option<(i64, String)> = execute_sql(&self.conn, self.deadline.as_ref(), || {
            self.conn
                .query_row(
                    "SELECT expires,payload FROM result_sets WHERE id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
        })?;
        self.ensure_live()?;
        let Some((expires, payload)) = row else {
            bail!(coded!(
                ExpiredResult,
                "result set expired or was evicted; search again"
            ));
        };
        if expires <= now() {
            bail!(coded!(ExpiredResult, "result set expired; search again"));
        }
        self.ensure_live()?;
        let set = serde_json::from_str(&payload).with_coded_context(|| {
            coded!(
                ResultUnavailable,
                "stored result format is invalid; search again"
            )
        })?;
        self.ensure_live()?;
        Ok(set)
    }

    fn ensure_live(&self) -> Result<()> {
        ensure_live(self.deadline.as_ref())
    }
}

fn busy_slice(deadline: Option<&QueryDeadline>) -> Duration {
    deadline.map_or(MAINTENANCE_BUSY_TIMEOUT, |deadline| {
        deadline.cap(SQL_BUSY_SLICE)
    })
}

/// Commits `tx` within one busy slice of the remaining budget.
fn commit(
    conn: &Connection,
    deadline: Option<&QueryDeadline>,
    tx: rusqlite::Transaction<'_>,
) -> Result<()> {
    ensure_live(deadline)?;
    conn.busy_timeout(busy_slice(deadline))
        .context("set result-set commit timeout")?;
    tx.commit()
        .map_err(|error| classify(deadline, error.into()))?;
    ensure_live(deadline)
}

fn classify(deadline: Option<&QueryDeadline>, error: anyhow::Error) -> anyhow::Error {
    match deadline {
        Some(deadline) if deadline.expired() => {
            error.with_coded_context(|| coded!(TimedOut, "query deadline expired"))
        }
        Some(deadline) => deadline.classify(error),
        None => error,
    }
}

fn ensure_live(deadline: Option<&QueryDeadline>) -> Result<()> {
    if deadline.is_some_and(QueryDeadline::expired) {
        bail!(coded!(TimedOut, "query deadline expired"));
    }
    Ok(())
}

fn execute_sql<T>(
    conn: &Connection,
    deadline: Option<&QueryDeadline>,
    mut operation: impl FnMut() -> rusqlite::Result<T>,
) -> Result<T> {
    let Some(deadline) = deadline else {
        return operation().context("execute result-set SQL");
    };
    loop {
        ensure_live(Some(deadline))?;
        conn.busy_timeout(deadline.cap(SQL_BUSY_SLICE))
            .map_err(|error| {
                deadline.classify(
                    anyhow::Error::new(error).context("set bounded result-set SQL timeout"),
                )
            })?;
        match operation() {
            Ok(value) => {
                ensure_live(Some(deadline))?;
                return Ok(value);
            }
            Err(error) if is_sqlite_busy(&error) => {
                ensure_live(Some(deadline))?;
                let pause = deadline.cap(BUSY_RETRY_PAUSE);
                if pause.is_zero() {
                    bail!(coded!(TimedOut, "query deadline expired"));
                }
                thread::sleep(pause);
            }
            Err(error) => {
                return Err(
                    deadline.classify(anyhow::Error::new(error).context("execute result-set SQL"))
                );
            }
        }
    }
}

fn is_sqlite_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::DatabaseBusy
    )
}

#[cfg(test)]
mod tests;
