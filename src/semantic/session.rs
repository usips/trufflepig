use super::{Embedding, SemanticEngine};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    io::ErrorKind,
    path::{Path, PathBuf},
};

/// Retains one CPU engine and its cross-process lease for a single root cache.
#[derive(Default)]
pub struct SemanticSession {
    // Declaration order releases model memory before releasing the inference lock.
    engine: Option<SemanticEngine>,
    lease: Option<InferenceLease>,
}

impl SemanticSession {
    pub fn new() -> Self {
        Self::default()
    }

    /// Embeds the query before callers open their index read snapshot.
    pub fn prepare(
        &mut self,
        enabled: bool,
        cache: &Path,
        text: &str,
    ) -> Result<Option<(&mut SemanticEngine, Embedding)>> {
        if !enabled {
            return Ok(None);
        }
        if !cfg!(feature = "semantic") {
            return Err(super::unavailable());
        }
        if let Some(lease) = &self.lease {
            lease.verify_cache(cache)?;
        }
        if self.engine.is_none() {
            let model_dir = std::env::var_os("TRUFFLEPIG_MODEL_DIR").context(
                "semantic_unavailable: set TRUFFLEPIG_MODEL_DIR to the pinned model assets",
            )?;
            let lease = InferenceLease::acquire(cache)?;
            let engine = SemanticEngine::open(Path::new(&model_dir), &lease.cache)?;
            self.engine = Some(engine);
            self.lease = Some(lease);
        }
        let engine = self
            .engine
            .as_mut()
            .context("semantic engine initialization failed")?;
        let query = engine.embed(text)?;
        Ok(Some((engine, query)))
    }
}

struct InferenceLease {
    cache: PathBuf,
    _lock: File,
}

impl InferenceLease {
    fn acquire(cache: &Path) -> Result<Self> {
        fs::create_dir_all(cache)?;
        let cache = fs::canonicalize(cache)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(cache.join("inference.lock"))?;
        if let Err(error) = FileExt::try_lock_exclusive(&lock) {
            if error.kind() == ErrorKind::WouldBlock {
                bail!(
                    "semantic_busy: another process owns this root's CPU inference session; use its daemon or retry after it exits"
                );
            }
            return Err(error).context("semantic_unavailable: cannot acquire inference lock");
        }
        Ok(Self { cache, _lock: lock })
    }

    fn verify_cache(&self, cache: &Path) -> Result<()> {
        if fs::canonicalize(cache)? != self.cache {
            bail!("semantic_cache_mismatch: a semantic session belongs to exactly one root cache");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_session_disabled_does_not_initialize() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let missing_cache = directory.path().join("absent");
        assert!(
            SemanticSession::new()
                .prepare(false, &missing_cache, "query")?
                .is_none()
        );
        assert!(!missing_cache.exists());
        Ok(())
    }

    #[test]
    fn semantic_inference_lease_is_exclusive_and_released() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let lease = InferenceLease::acquire(directory.path())?;
        let error = InferenceLease::acquire(directory.path())
            .err()
            .context("second lease succeeded")?;
        assert!(error.to_string().starts_with("semantic_busy:"));
        drop(lease);
        assert!(InferenceLease::acquire(directory.path()).is_ok());
        Ok(())
    }

    #[test]
    fn semantic_inference_lease_rejects_another_root() -> Result<()> {
        let first = tempfile::tempdir()?;
        let second = tempfile::tempdir()?;
        let lease = InferenceLease::acquire(first.path())?;
        lease.verify_cache(&first.path().join("."))?;
        assert!(
            lease
                .verify_cache(second.path())
                .unwrap_err()
                .to_string()
                .starts_with("semantic_cache_mismatch:")
        );
        Ok(())
    }
}
