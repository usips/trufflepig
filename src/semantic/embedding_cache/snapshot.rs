use super::cache_format;
#[cfg(test)]
use super::MAX_BATCH_ENTRIES;
use crate::semantic::Embedding;
use anyhow::Result;
#[cfg(test)]
use anyhow::bail;
use rusqlite::{Connection, OpenFlags};
#[cfg(test)]
use rusqlite::params_from_iter;
#[cfg(test)]
use std::collections::HashMap;
use std::{path::Path, sync::Arc, time::Duration};

/// A read transaction that keeps all cache lookups on one SQLite snapshot.
pub(crate) struct EmbeddingCacheSnapshot {
    db: Connection,
    counters: Arc<cache_format::CacheCounters>,
    manifest_digest: String,
}

impl EmbeddingCacheSnapshot {
    pub(crate) fn get(&self, key: &str) -> Result<Option<Embedding>> {
        let mut statement = self.db.prepare_cached(
            "SELECT e.vector,p.model_revision,p.input_version,p.dimensions,
                    p.normalization,p.manifest_digest
             FROM embeddings e
             LEFT JOIN embedding_provenance p ON p.key=e.key
             WHERE e.key=?1",
        )?;
        let row = statement.query_row([key], |row| {
            let bytes = match row.get::<_, Vec<u8>>(0) {
                Ok(bytes) => bytes,
                Err(_) => {
                    self.counters.corrupt_miss();
                    return Ok(None);
                }
            };
            Ok(cache_format::decode(
                &bytes,
                row.get::<_, Option<String>>(1).ok().flatten().as_deref(),
                row.get::<_, Option<String>>(2).ok().flatten().as_deref(),
                row.get::<_, Option<i64>>(3).ok().flatten(),
                row.get::<_, Option<String>>(4).ok().flatten().as_deref(),
                row.get::<_, Option<String>>(5).ok().flatten().as_deref(),
                &self.manifest_digest,
                &self.counters,
            ))
        });
        match row {
            Ok(vector) => Ok(vector),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    #[cfg(test)]
    pub(crate) fn get_many<I, S>(&self, keys: I) -> Result<Vec<Option<Embedding>>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let keys = collect_keys(keys)?;
        let mut result = vec![None; keys.len()];
        for (chunk_number, chunk) in keys.chunks(MAX_BATCH_ENTRIES).enumerate() {
            let offset = chunk_number * MAX_BATCH_ENTRIES;
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT e.key,e.vector,p.model_revision,p.input_version,p.dimensions,
                        p.normalization,p.manifest_digest
                 FROM embeddings e
                 LEFT JOIN embedding_provenance p ON p.key=e.key
                 WHERE e.key IN ({placeholders})"
            );
            let mut statement = self.db.prepare(&sql)?;
            let mut rows = statement.query(params_from_iter(chunk.iter().map(String::as_str)))?;
            let mut positions = HashMap::<String, Vec<usize>>::with_capacity(chunk.len());
            for (index, key) in chunk.iter().enumerate() {
                positions
                    .entry(key.clone())
                    .or_default()
                    .push(offset + index);
            }
            while let Some(row) = rows.next()? {
                let key: String = row.get(0)?;
                let bytes = match row.get::<_, Vec<u8>>(1) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        self.counters.corrupt_miss();
                        continue;
                    }
                };
                let vector = cache_format::decode(
                    &bytes,
                    row.get::<_, Option<String>>(2).ok().flatten().as_deref(),
                    row.get::<_, Option<String>>(3).ok().flatten().as_deref(),
                    row.get::<_, Option<i64>>(4).ok().flatten(),
                    row.get::<_, Option<String>>(5).ok().flatten().as_deref(),
                    row.get::<_, Option<String>>(6).ok().flatten().as_deref(),
                    &self.manifest_digest,
                    &self.counters,
                );
                if let Some(indices) = positions.get(&key) {
                    for index in indices {
                        result[*index] = vector.clone();
                    }
                }
            }
        }
        Ok(result)
    }

    pub(crate) fn status(&self) -> cache_format::CacheStatus {
        self.counters.status()
    }
}

pub(super) fn open(
    path: &Path,
    counters: Arc<cache_format::CacheCounters>,
    manifest_digest: String,
    busy_timeout: Duration,
) -> Result<EmbeddingCacheSnapshot> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(busy_timeout)?;
    // A deferred transaction acquires its snapshot on the first read. Touch the
    // table here so later reads keep the view observed at begin_snapshot time.
    db.execute_batch("PRAGMA query_only=ON; BEGIN; SELECT count(*) FROM embeddings;")?;
    Ok(EmbeddingCacheSnapshot {
        db,
        counters,
        manifest_digest,
    })
}

#[cfg(test)]
fn collect_keys<I, S>(keys: I) -> Result<Vec<String>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut collected = Vec::new();
    for key in keys {
        if collected.len() == MAX_BATCH_ENTRIES {
            bail!("embedding cache batch exceeds {MAX_BATCH_ENTRIES} entries");
        }
        collected.push(key.as_ref().to_owned());
    }
    Ok(collected)
}

impl Drop for EmbeddingCacheSnapshot {
    fn drop(&mut self) {
        let _ = self.db.execute_batch("ROLLBACK;");
    }
}
