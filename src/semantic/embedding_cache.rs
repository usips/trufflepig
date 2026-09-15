mod cache_format;
mod snapshot;

use super::{CACHE_BYTES, DIMENSIONS, INPUT_VERSION, MODEL_REVISION};
use anyhow::{Result, bail};
use cache_format::{CacheCounters, NORMALIZATION, encode, manifest_digest};
use rusqlite::{Connection, OpenFlags, params, params_from_iter};
use std::{
    borrow::Borrow,
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

pub(crate) use cache_format::content_key;
#[cfg(test)]
use cache_format::CacheStatus;
pub(crate) use snapshot::EmbeddingCacheSnapshot;

const PAGE_SIZE: u64 = 4096;
const MIN_LIMIT: u64 = 64 * 1024;
const MAX_BATCH_ENTRIES: usize = 4096;
const DELETE_CHUNK: usize = 256;
const SCHEMA_VERSION: i64 = 2;

/// Evictable content cache; SQLite capacity includes table and index overhead.
pub(crate) struct EmbeddingCache {
    db: Connection,
    database_path: PathBuf,
    max_entries: u64,
    manifest_digest: String,
    counters: Arc<CacheCounters>,
    read_busy_timeout: Duration,
}

impl EmbeddingCache {
    pub(crate) fn open(directory: &Path) -> Result<Self> {
        Self::with_limit(directory, CACHE_BYTES)
    }

    fn with_limit(directory: &Path, limit: u64) -> Result<Self> {
        if limit < MIN_LIMIT {
            bail!("embedding cache limit must be at least 64 KiB");
        }
        fs::create_dir_all(directory)?;
        let database_path = directory.join("embeddings.sqlite");
        let db = Connection::open(&database_path)?;
        db.busy_timeout(Duration::from_secs(10))?;
        let schema_version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let reset_schema = schema_version != SCHEMA_VERSION || !has_current_schema(&db)?;
        if reset_schema {
            // This cache is disposable. Reset old layouts so every row has the
            // complete immutable provenance contract below.
            db.execute_batch(
                "DROP TABLE IF EXISTS embedding_provenance;
                 DROP TABLE IF EXISTS embeddings;
                 DROP TABLE IF EXISTS cache_clock;",
            )?;
        }
        if reset_schema {
            db.execute_batch(
                "PRAGMA page_size=4096;
                 PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=NORMAL;
                 PRAGMA cache_size=-2048;
                 PRAGMA auto_vacuum=INCREMENTAL;
                 PRAGMA wal_autocheckpoint=256;
                 CREATE TABLE IF NOT EXISTS embeddings(
                     key TEXT PRIMARY KEY, vector BLOB NOT NULL, touched INTEGER NOT NULL);
                 CREATE TABLE IF NOT EXISTS embedding_provenance(
                     key TEXT PRIMARY KEY,
                     model_revision TEXT NOT NULL,
                     input_version TEXT NOT NULL,
                     dimensions INTEGER NOT NULL,
                     normalization TEXT NOT NULL,
                     manifest_digest TEXT NOT NULL);
                 CREATE INDEX IF NOT EXISTS embedding_recency ON embeddings(touched, key);
                 CREATE TABLE IF NOT EXISTS cache_clock(
                     id INTEGER PRIMARY KEY CHECK(id=1), tick INTEGER NOT NULL);
                 INSERT OR IGNORE INTO cache_clock VALUES(1,0);",
            )?;
        } else {
            db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        }
        if reset_schema {
            db.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        db.pragma_update(None, "max_page_count", (limit / PAGE_SIZE) as i64)?;
        let max_pages: i64 = db.pragma_query_value(None, "max_page_count", |row| row.get(0))?;
        // A vector and its provenance/index entries each consume at least one page.
        let max_entries = (max_pages.max(0) as u64 / 2).saturating_sub(8).max(1);
        Ok(Self {
            db,
            database_path,
            max_entries,
            manifest_digest: manifest_digest(),
            counters: Arc::new(CacheCounters::default()),
            read_busy_timeout: Duration::from_secs(10),
        })
    }

    /// Opens an existing cache for a query without changing its schema.
    ///
    /// Query preparation must not wait on the background writer or perform
    /// DDL. A missing or locked cache is treated as a cache miss by callers.
    pub(crate) fn open_query(directory: &Path) -> Result<Self> {
        Self::open_existing(directory, OpenFlags::SQLITE_OPEN_READ_ONLY, Duration::ZERO)
    }

    /// Opens an existing cache for best-effort query persistence.
    ///
    /// This path lazily initializes an empty cache, never upgrades an existing
    /// schema, and returns promptly when the preparation writer owns the lock.
    pub(crate) fn open_query_writer(directory: &Path) -> Result<Self> {
        fs::create_dir_all(directory)?;
        let database_path = directory.join("embeddings.sqlite");
        let db = Connection::open_with_flags(
            &database_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        db.busy_timeout(Duration::ZERO)?;
        let schema_version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if schema_version == SCHEMA_VERSION && has_current_schema(&db)? {
            db.pragma_update(None, "max_page_count", (CACHE_BYTES / PAGE_SIZE) as i64)?;
            return Self::from_existing(db, database_path, Duration::ZERO);
        }
        let tables: i64 = db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type IN ('table','index')",
            [],
            |row| row.get(0),
        )?;
        if schema_version != 0 || tables != 0 {
            bail!("semantic_cache_unavailable: cache schema is not ready");
        }
        db.execute_batch(
            "PRAGMA page_size=4096;
             PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA cache_size=-2048;
             PRAGMA auto_vacuum=INCREMENTAL;
             PRAGMA wal_autocheckpoint=256;
             CREATE TABLE IF NOT EXISTS embeddings(
                 key TEXT PRIMARY KEY, vector BLOB NOT NULL, touched INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS embedding_provenance(
                 key TEXT PRIMARY KEY,
                 model_revision TEXT NOT NULL,
                 input_version TEXT NOT NULL,
                 dimensions INTEGER NOT NULL,
                 normalization TEXT NOT NULL,
                 manifest_digest TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS embedding_recency ON embeddings(touched, key);
             CREATE TABLE IF NOT EXISTS cache_clock(
                 id INTEGER PRIMARY KEY CHECK(id=1), tick INTEGER NOT NULL);
             INSERT OR IGNORE INTO cache_clock VALUES(1,0);",
        )?;
        db.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        db.pragma_update(None, "max_page_count", (CACHE_BYTES / PAGE_SIZE) as i64)?;
        Self::from_existing(db, database_path, Duration::ZERO)
    }

    fn open_existing(
        directory: &Path,
        flags: OpenFlags,
        read_busy_timeout: Duration,
    ) -> Result<Self> {
        let database_path = directory.join("embeddings.sqlite");
        let db = Connection::open_with_flags(&database_path, flags)?;
        db.busy_timeout(read_busy_timeout)?;
        let schema_version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if schema_version != SCHEMA_VERSION || !has_current_schema(&db)? {
            bail!("semantic_cache_unavailable: cache schema is not ready");
        }
        Self::from_existing(db, database_path, read_busy_timeout)
    }

    fn from_existing(
        db: Connection,
        database_path: PathBuf,
        read_busy_timeout: Duration,
    ) -> Result<Self> {
        let max_pages: i64 = db.pragma_query_value(None, "max_page_count", |row| row.get(0))?;
        let max_entries = (max_pages.max(0) as u64 / 2).saturating_sub(8).max(1);
        Ok(Self {
            db,
            database_path,
            max_entries,
            manifest_digest: manifest_digest(),
            counters: Arc::new(CacheCounters::default()),
            read_busy_timeout,
        })
    }

    /// Opens a stable read transaction for cached-only candidate retrieval.
    pub(crate) fn begin_snapshot(&self) -> Result<EmbeddingCacheSnapshot> {
        snapshot::open(
            &self.database_path,
            Arc::clone(&self.counters),
            self.manifest_digest.clone(),
            self.read_busy_timeout,
        )
    }

    pub(crate) fn with_snapshot<T, F>(&self, read: F) -> Result<T>
    where
        F: FnOnce(&EmbeddingCacheSnapshot) -> Result<T>,
    {
        let snapshot = self.begin_snapshot()?;
        read(&snapshot)
    }

    pub(crate) fn get(&self, key: &str) -> Result<Option<super::Embedding>> {
        self.with_snapshot(|snapshot| snapshot.get(key))
    }

    #[cfg(test)]
    pub(crate) fn get_many<I, S>(&self, keys: I) -> Result<Vec<Option<super::Embedding>>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.with_snapshot(|snapshot| snapshot.get_many(keys))
    }

    #[cfg(test)]
    pub(crate) fn status(&self) -> CacheStatus {
        self.counters.status()
    }

    pub(crate) fn put(&mut self, key: &str, vector: &super::Embedding) -> Result<()> {
        self.put_many(std::iter::once((key, vector)))
    }

    /// Writes one bounded batch and updates count/recency metadata once.
    pub(crate) fn put_many<I, E>(&mut self, entries: I) -> Result<()>
    where
        I: IntoIterator<Item = E>,
        E: CachePutItem,
    {
        let mut writes = Vec::new();
        for entry in entries {
            if writes.len() == MAX_BATCH_ENTRIES {
                bail!("embedding cache batch exceeds {MAX_BATCH_ENTRIES} entries");
            }
            writes.push(CacheWrite {
                key: entry.key().to_owned(),
                bytes: encode(entry.vector())?,
            });
        }
        if writes.is_empty() {
            return Ok(());
        }
        deduplicate_latest(&mut writes);
        let keep = (self.max_entries as usize).min(writes.len());
        if writes.len() > keep {
            writes.drain(..writes.len() - keep);
        }

        let tx = self.db.transaction()?;
        // Remove replacements first. This makes capacity accounting independent of
        // whether eviction happens to select an incoming key.
        for chunk in writes.chunks(DELETE_CHUNK) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!("DELETE FROM embeddings WHERE key IN ({placeholders})");
            tx.execute(
                &sql,
                params_from_iter(chunk.iter().map(|write| write.key.as_str())),
            )?;
            let sql = format!("DELETE FROM embedding_provenance WHERE key IN ({placeholders})");
            tx.execute(
                &sql,
                params_from_iter(chunk.iter().map(|write| write.key.as_str())),
            )?;
        }
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM embeddings", [], |row| row.get(0))?;
        let required = (count as u64)
            .saturating_add(writes.len() as u64)
            .saturating_sub(self.max_entries) as i64;
        if required > 0 {
            tx.execute(
                "DELETE FROM embeddings WHERE key IN
                 (SELECT key FROM embeddings ORDER BY touched,key LIMIT ?1)",
                [required],
            )?;
            tx.execute(
                "DELETE FROM embedding_provenance WHERE key NOT IN (SELECT key FROM embeddings)",
                [],
            )?;
        }
        tx.execute(
            "UPDATE cache_clock SET tick=tick+?1 WHERE id=1",
            [writes.len() as i64],
        )?;
        let tick: i64 = tx.query_row("SELECT tick FROM cache_clock WHERE id=1", [], |row| {
            row.get(0)
        })?;
        for (index, write) in writes.iter().enumerate() {
            let touched = tick - (writes.len() - index - 1) as i64;
            tx.execute(
                "INSERT OR REPLACE INTO embeddings(key,vector,touched) VALUES(?1,?2,?3)",
                params![write.key, write.bytes.as_slice(), touched],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO embedding_provenance
                 (key,model_revision,input_version,dimensions,normalization,manifest_digest)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    write.key,
                    MODEL_REVISION,
                    INPUT_VERSION,
                    DIMENSIONS as i64,
                    NORMALIZATION,
                    self.manifest_digest,
                ],
            )?;
        }
        Ok(tx.commit()?)
    }
}

fn has_current_schema(db: &Connection) -> Result<bool> {
    let mut tables = HashSet::new();
    let mut statement = db.prepare("SELECT name FROM sqlite_master WHERE type='table'")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        tables.insert(row.get::<_, String>(0)?);
    }
    if !["embeddings", "embedding_provenance", "cache_clock"]
        .iter()
        .all(|name| tables.contains(*name))
    {
        return Ok(false);
    }
    let required = [
        "key",
        "model_revision",
        "input_version",
        "dimensions",
        "normalization",
        "manifest_digest",
    ];
    let mut columns = HashSet::new();
    let mut statement = db.prepare("PRAGMA table_info(embedding_provenance)")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        columns.insert(row.get::<_, String>(1)?);
    }
    Ok(required.iter().all(|name| columns.contains(*name)))
}

pub(crate) trait CachePutItem {
    fn key(&self) -> &str;
    fn vector(&self) -> &super::Embedding;
}

impl<K, V> CachePutItem for (K, V)
where
    K: AsRef<str>,
    V: Borrow<super::Embedding>,
{
    fn key(&self) -> &str {
        self.0.as_ref()
    }

    fn vector(&self) -> &super::Embedding {
        self.1.borrow()
    }
}

impl<K, V> CachePutItem for &(K, V)
where
    K: AsRef<str>,
    V: Borrow<super::Embedding>,
{
    fn key(&self) -> &str {
        self.0.as_ref()
    }

    fn vector(&self) -> &super::Embedding {
        self.1.borrow()
    }
}

struct CacheWrite {
    key: String,
    bytes: [u8; DIMENSIONS * 4],
}

fn deduplicate_latest(writes: &mut Vec<CacheWrite>) {
    let mut seen = HashSet::with_capacity(writes.len());
    let mut unique = Vec::with_capacity(writes.len());
    while let Some(write) = writes.pop() {
        if seen.insert(write.key.clone()) {
            unique.push(write);
        }
    }
    unique.reverse();
    *writes = unique;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn vector(axis: usize) -> super::super::Embedding {
        let mut values = [0.0; DIMENSIONS];
        values[axis] = 1.0;
        super::super::Embedding(values)
    }

    #[test]
    fn semantic_cache_survives_restart_and_evicts() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let first = vector(0);
        let mut cache = EmbeddingCache::with_limit(dir.path(), MIN_LIMIT)?;
        cache.put("first", &first)?;
        drop(cache);
        let mut cache = EmbeddingCache::with_limit(dir.path(), MIN_LIMIT)?;
        assert!(cache.get("first")?.is_some());
        cache.put("second", &first)?;
        assert!(cache.get("first")?.is_none());
        assert!(cache.get("second")?.is_some());
        let provenance: (String, String, String, i64, String, String) = cache.db.query_row(
            "SELECT key,model_revision,input_version,dimensions,normalization,manifest_digest
             FROM embedding_provenance",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )?;
        assert_eq!(provenance.0, "second");
        assert_eq!(provenance.1, MODEL_REVISION);
        assert_eq!(provenance.2, INPUT_VERSION);
        assert_eq!(provenance.3, DIMENSIONS as i64);
        assert_eq!(provenance.4, NORMALIZATION);
        assert_eq!(provenance.5, manifest_digest());
        let count: i64 =
            cache
                .db
                .query_row("SELECT count(*) FROM embedding_provenance", [], |row| {
                    row.get(0)
                })?;
        assert_eq!(count, 1);
        assert!(fs::metadata(dir.path().join("embeddings.sqlite"))?.len() <= MIN_LIMIT);
        Ok(())
    }

    #[test]
    fn semantic_cache_rejects_raw_corruption_and_provenance_mismatch() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut cache = EmbeddingCache::with_limit(dir.path(), MIN_LIMIT * 2)?;
        let key = "corrupt";
        cache.put(key, &vector(0))?;
        cache.db.execute(
            "UPDATE embeddings SET vector=?1 WHERE key=?2",
            params![vec![0_u8; DIMENSIONS * 4], key],
        )?;
        assert!(cache.get(key)?.is_none());
        assert_eq!(cache.status().corrupt_misses, 1);
        cache.put(key, &vector(0))?;
        let mut wrong_norm = vec![0_u8; DIMENSIONS * 4];
        wrong_norm[..4].copy_from_slice(&2.0_f32.to_le_bytes());
        cache.db.execute(
            "UPDATE embeddings SET vector=?1 WHERE key=?2",
            params![wrong_norm, key],
        )?;
        assert!(cache.get(key)?.is_none());
        assert_eq!(cache.status().corrupt_misses, 2);
        cache.put(key, &vector(0))?;
        let mut nonfinite = vec![0_u8; DIMENSIONS * 4];
        nonfinite[..4].copy_from_slice(&f32::NAN.to_le_bytes());
        cache.db.execute(
            "UPDATE embeddings SET vector=?1 WHERE key=?2",
            params![nonfinite, key],
        )?;
        assert!(cache.get(key)?.is_none());
        assert_eq!(cache.status().corrupt_misses, 3);
        cache.put(key, &vector(0))?;
        cache.db.execute(
            "UPDATE embedding_provenance SET input_version='old' WHERE key=?1",
            [key],
        )?;
        assert!(cache.get(key)?.is_none());
        assert_eq!(cache.status().provenance_misses, 1);
        Ok(())
    }

    #[test]
    fn semantic_cache_batches_reads_and_writes_in_one_snapshot() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut cache = EmbeddingCache::with_limit(dir.path(), MIN_LIMIT * 32)?;
        let first = vector(0);
        let second = vector(1);
        cache.put_many([("a", &first), ("b", &second), ("a", &second)])?;
        let values = cache.get_many(["a", "b", "missing", "a"])?;
        assert_eq!(values.len(), 4);
        assert_eq!(values[0].as_ref().unwrap().0, second.0);
        assert_eq!(values[1].as_ref().unwrap().0, second.0);
        assert!(values[2].is_none());
        assert_eq!(values[3].as_ref().unwrap().0, second.0);
        Ok(())
    }

    #[test]
    fn semantic_cache_get_does_not_write_recency() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut cache = EmbeddingCache::with_limit(dir.path(), MIN_LIMIT * 32)?;
        cache.put("stable", &vector(0))?;
        let before: i64 =
            cache
                .db
                .query_row("SELECT tick FROM cache_clock WHERE id=1", [], |row| {
                    row.get(0)
                })?;
        assert!(cache.get("stable")?.is_some());
        let after: i64 =
            cache
                .db
                .query_row("SELECT tick FROM cache_clock WHERE id=1", [], |row| {
                    row.get(0)
                })?;
        assert_eq!(before, after);
        Ok(())
    }

    #[test]
    fn semantic_cache_snapshots_are_concurrent_with_writes() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut cache = EmbeddingCache::with_limit(dir.path(), MIN_LIMIT * 32)?;
        cache.put("stable", &vector(0))?;
        let before_write = cache.begin_snapshot()?;
        cache.put("after", &vector(1))?;
        assert!(before_write.get("after")?.is_none());
        let directory = dir.path().to_path_buf();
        let handle = thread::spawn(move || {
            let cache = EmbeddingCache::open(&directory).unwrap();
            let snapshot = cache.begin_snapshot().unwrap();
            snapshot.get("stable").unwrap().is_some()
        });
        cache.put("new", &vector(1))?;
        assert!(handle.join().unwrap());
        Ok(())
    }

    #[test]
    fn query_cache_open_does_not_wait_for_a_writer() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut cache = EmbeddingCache::with_limit(dir.path(), MIN_LIMIT * 2)?;
        cache.put("stable", &vector(0))?;
        drop(cache);
        let lock = Connection::open(dir.path().join("embeddings.sqlite"))?;
        lock.execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE;")?;

        let started = std::time::Instant::now();
        let result = EmbeddingCache::open_query(dir.path());
        assert!(started.elapsed() < Duration::from_millis(250));
        assert!(result.is_err());
        let started = std::time::Instant::now();
        let result = EmbeddingCache::open_query_writer(dir.path());
        assert!(started.elapsed() < Duration::from_millis(250));
        assert!(result.is_err());
        drop(lock);
        Ok(())
    }

    #[test]
    fn query_cache_writer_initializes_an_empty_cache() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let vector = vector(0);
        let mut cache = EmbeddingCache::open_query_writer(dir.path())?;
        cache.put("first", &vector)?;
        drop(cache);
        let cache = EmbeddingCache::open_query(dir.path())?;
        assert!(cache.get("first")?.is_some());
        drop(cache);
        let writer = EmbeddingCache::open_query_writer(dir.path())?;
        let pages: i64 = writer
            .db
            .pragma_query_value(None, "max_page_count", |row| row.get(0))?;
        assert_eq!(pages, (CACHE_BYTES / PAGE_SIZE) as i64);
        assert!(writer.max_entries < CACHE_BYTES / PAGE_SIZE);
        Ok(())
    }

    #[test]
    fn content_key_binds_provenance_and_content() {
        assert_eq!(
            cache_format::content_key("same"),
            cache_format::content_key("same")
        );
        assert_ne!(
            cache_format::content_key("same"),
            cache_format::content_key("changed")
        );
        assert_ne!(cache_format::content_key("same").len(), 0);
    }
}
