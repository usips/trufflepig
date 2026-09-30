//! `<cache>/results.sqlite3`: persisted result sets, apart from the published
//! index so saving a query's results never waits behind a publication.
use super::{MAX_BYTES, MAX_HITS, StoredResultSet, now};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

const MAX_SETS: usize = 50;
const TTL_SECONDS: i64 = 600;
pub const RESULTS_DATABASE: &str = "results.sqlite3";

/// One cache's result-set database; the only writer path a read query uses.
pub struct ResultSetStore {
    pub(crate) conn: Connection,
}

impl ResultSetStore {
    pub fn open(cache: &Path) -> Result<Self> {
        std::fs::create_dir_all(cache)?;
        let conn = Connection::open(cache.join(RESULTS_DATABASE))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA journal_size_limit=4194304;
             CREATE TABLE IF NOT EXISTS result_sets(id TEXT PRIMARY KEY, created INTEGER NOT NULL, expires INTEGER NOT NULL, payload TEXT NOT NULL)",
        )?;
        Ok(Self { conn })
    }

    /// Assigns handles, bounds the set to [`MAX_HITS`] and [`MAX_BYTES`], and
    /// evicts the oldest other sets past the retention limits.
    pub fn save(&self, mut set: StoredResultSet) -> Result<String> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        if set.hits.len() > MAX_HITS {
            set.hits.truncate(MAX_HITS);
            set.truncated = true;
        }
        for (ordinal, hit) in set.hits.iter_mut().enumerate() {
            *hit.handle_mut() = format!("{id}:{}", ordinal + 1);
        }
        let mut payload = serde_json::to_string(&set)?;
        while payload.len() > MAX_BYTES && !set.hits.is_empty() {
            let keep = (set.hits.len() * MAX_BYTES / payload.len()).saturating_sub(1);
            set.hits.truncate(keep);
            set.truncated = true;
            payload = serde_json::to_string(&set)?;
        }
        if payload.len() > MAX_BYTES {
            bail!("result_cache_unavailable: metadata exceeds cache capacity");
        }
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let time = now();
        tx.execute("DELETE FROM result_sets WHERE expires<=?1", [time])?;
        tx.execute(
            "INSERT INTO result_sets VALUES(?1,?2,?3,?4)",
            params![id, time, time + TTL_SECONDS, payload],
        )?;
        loop {
            let (count, bytes): (usize, usize) = tx.query_row(
                "SELECT count(*),coalesce(sum(length(CAST(payload AS BLOB))),0) FROM result_sets",
                [],
                |r| Ok((r.get::<_, i64>(0)? as usize, r.get::<_, i64>(1)? as usize)),
            )?;
            if count <= MAX_SETS && bytes <= MAX_BYTES {
                break;
            }
            tx.execute("DELETE FROM result_sets WHERE id=(SELECT id FROM result_sets WHERE id != ?1 ORDER BY created,id LIMIT 1)", [&id])?;
        }
        tx.commit()?;
        Ok(id)
    }

    pub fn load(&self, id: &str) -> Result<StoredResultSet> {
        uuid::Uuid::parse_str(id)
            .context("invalid_handle: expected immutable result-set identifier")?;
        let row: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT expires,payload FROM result_sets WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((expires, payload)) = row else {
            bail!("expired_result: result set expired or was evicted; search again");
        };
        if expires <= now() {
            bail!("expired_result: result set expired; search again");
        }
        serde_json::from_str(&payload)
            .context("result_unavailable: stored result format is invalid; search again")
    }
}
