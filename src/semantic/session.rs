//! Query preparation precedes source snapshots; root processes never own a model.
use super::Embedding;
use anyhow::Result;
use std::path::Path;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ResidencySample {
    pub loaded: bool,
    pub idle_seconds: u64,
    pub process_resident_bytes: Option<u64>,
    pub unloaded: bool,
}

#[derive(Default)]
pub struct SemanticSession {
    no_daemon: bool,
}

impl SemanticSession {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set_no_daemon(&mut self, no_daemon: bool) {
        self.no_daemon = no_daemon;
    }
    pub fn is_loaded(&self) -> bool {
        false
    }
    pub fn initializations(&self) -> u64 {
        0
    }

    pub fn prepare(
        &mut self,
        enabled: bool,
        cache: &Path,
        text: &str,
    ) -> Result<Option<Embedding>> {
        if !enabled {
            return Ok(None);
        }
        self.query(cache, text).map(Some)
    }

    #[cfg(feature = "semantic")]
    fn query(&self, directory: &Path, text: &str) -> Result<Embedding> {
        use super::embedding_cache::{EmbeddingCache, content_key};
        let key = content_key(text);
        let mut cache_locked = false;
        match EmbeddingCache::open_query(directory) {
            Ok(cache) => match cache.get(&key) {
                Ok(Some(vector)) => return Ok(vector),
                Ok(None) => {}
                Err(error) if is_cache_lock(&error) => cache_locked = true,
                Err(_) => {}
            },
            Err(error) => cache_locked = is_cache_lock(&error),
        }
        if self.no_daemon {
            if cache_locked {
                anyhow::bail!("semantic_cache_locked: cached query is temporarily unavailable");
            }
            anyhow::bail!("semantic_pending: query vector is not cached in --no-daemon mode");
        }
        let vector = super::worker::embed_query(directory, text)?;
        // Inference has already met the query deadline. Persistence is an
        // optimization and must not turn a valid vector into a failed query.
        if let Ok(mut cache) = EmbeddingCache::open_query_writer(directory) {
            let _ = cache.put(&key, &vector);
        }
        Ok(vector)
    }

    #[cfg(not(feature = "semantic"))]
    fn query(&self, _: &Path, _: &str) -> Result<Embedding> {
        Err(super::unavailable())
    }
}

#[cfg(feature = "semantic")]
fn is_cache_lock(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<rusqlite::Error>()
            .is_some_and(|error| {
                matches!(
                    error,
                    rusqlite::Error::SqliteFailure(code, _)
                        if matches!(
                            code.code,
                            rusqlite::ErrorCode::DatabaseBusy
                                | rusqlite::ErrorCode::DatabaseLocked
                        )
                )
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_disabled_does_not_create_cache() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let absent = directory.path().join("absent");
        assert!(
            SemanticSession::new()
                .prepare(false, &absent, "query")?
                .is_none()
        );
        assert!(!absent.exists());
        Ok(())
    }

    #[cfg(feature = "semantic")]
    #[test]
    fn semantic_no_daemon_reads_cached_query_without_model() -> Result<()> {
        use super::super::embedding_cache::{EmbeddingCache, content_key};
        let directory = tempfile::tempdir()?;
        let mut values = [0.0; super::super::DIMENSIONS];
        values[0] = 1.0;
        EmbeddingCache::open(directory.path())?.put(&content_key("cached"), &Embedding(values))?;
        let mut session = SemanticSession::new();
        session.set_no_daemon(true);
        assert_eq!(
            session
                .prepare(true, directory.path(), "cached")?
                .unwrap()
                .0[0],
            1.0
        );
        assert!(
            session
                .prepare(true, directory.path(), "missing")
                .unwrap_err()
                .to_string()
                .contains("semantic_pending")
        );
        assert!(!session.is_loaded());
        Ok(())
    }
}
