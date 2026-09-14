use super::{CACHE_BYTES, DIMENSIONS, Embedding};
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use std::{fs, path::Path, time::Duration};

/// Evictable content cache; the hard SQLite page limit includes table/index overhead.
pub(super) struct EmbeddingCache {
    db: Connection,
    max_entries: u64,
}

impl EmbeddingCache {
    pub(super) fn open(directory: &Path) -> Result<Self> {
        Self::with_limit(directory, CACHE_BYTES)
    }

    fn with_limit(directory: &Path, limit: u64) -> Result<Self> {
        if limit < 64 * 1024 {
            bail!("embedding cache limit must be at least 64 KiB");
        }
        fs::create_dir_all(directory)?;
        let db = Connection::open(directory.join("embeddings.sqlite"))?;
        db.busy_timeout(Duration::from_secs(10))?;
        db.execute_batch("PRAGMA page_size=4096; PRAGMA journal_mode=DELETE;
            PRAGMA cache_size=-2048; PRAGMA auto_vacuum=INCREMENTAL;
            CREATE TABLE IF NOT EXISTS embeddings(
                key TEXT PRIMARY KEY, vector BLOB NOT NULL, touched INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS embedding_recency ON embeddings(touched, key);
            CREATE TABLE IF NOT EXISTS cache_clock(id INTEGER PRIMARY KEY CHECK(id=1), tick INTEGER NOT NULL);
            INSERT OR IGNORE INTO cache_clock VALUES(1,0);")?;
        db.pragma_update(None, "max_page_count", (limit / 4096) as i64)?;
        // Two pages per vector leaves room for indexes and bounded rollback journals.
        let max_entries = (limit / 8192).saturating_sub(8).max(1);
        Ok(Self { db, max_entries })
    }

    pub(super) fn get(&mut self, key: &str) -> Result<Option<Embedding>> {
        let tx = self.db.transaction()?;
        let bytes: Option<Vec<u8>> = tx
            .query_row("SELECT vector FROM embeddings WHERE key=?1", [key], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        if bytes.len() != DIMENSIONS * 4 {
            bail!("invalid cached embedding length");
        }
        let mut values = [0.0; DIMENSIONS];
        for (value, chunk) in values.iter_mut().zip(bytes.as_chunks::<4>().0) {
            *value = f32::from_le_bytes(*chunk);
        }
        let vector = Embedding::from_values(&values)?;
        tx.execute("UPDATE cache_clock SET tick=tick+1 WHERE id=1", [])?;
        tx.execute(
            "UPDATE embeddings SET touched=(SELECT tick FROM cache_clock WHERE id=1) WHERE key=?1",
            [key],
        )?;
        tx.commit()?;
        Ok(Some(vector))
    }

    pub(super) fn put(&mut self, key: &str, vector: &Embedding) -> Result<()> {
        let tx = self.db.transaction()?;
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM embeddings", [], |row| row.get(0))?;
        if count >= self.max_entries as i64 {
            // Evict in batches rather than growing/reclaiming one row per insertion.
            let remove = (self.max_entries / 100).max(1);
            tx.execute(
                "DELETE FROM embeddings WHERE key IN
                (SELECT key FROM embeddings ORDER BY touched,key LIMIT ?1)",
                [remove as i64],
            )?;
        }
        let mut bytes = [0_u8; DIMENSIONS * 4];
        for (chunk, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(vector.0.iter()) {
            chunk.copy_from_slice(&value.to_le_bytes());
        }
        tx.execute("UPDATE cache_clock SET tick=tick+1 WHERE id=1", [])?;
        tx.execute(
            "INSERT OR REPLACE INTO embeddings(key,vector,touched)
            VALUES(?1,?2,(SELECT tick FROM cache_clock WHERE id=1))",
            params![key, bytes.as_slice()],
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn semantic_cache_survives_restart_and_evicts() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let vector = Embedding::from_values(&[1.0; DIMENSIONS])?;
        let mut cache = EmbeddingCache::with_limit(dir.path(), 64 * 1024)?;
        cache.put("first", &vector)?;
        drop(cache);
        let mut cache = EmbeddingCache::with_limit(dir.path(), 64 * 1024)?;
        assert!(cache.get("first")?.is_some());
        cache.put("second", &vector)?;
        assert!(cache.get("first")?.is_none());
        assert!(cache.get("second")?.is_some());
        assert!(fs::metadata(dir.path().join("embeddings.sqlite"))?.len() <= 64 * 1024);
        Ok(())
    }
}
