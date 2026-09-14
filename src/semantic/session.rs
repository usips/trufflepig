use super::{Embedding, SemanticEngine};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    io::ErrorKind,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const IDLE_UNLOAD: Duration = Duration::from_secs(10 * 60);
const RESIDENCY_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ResidencySample {
    pub loaded: bool,
    pub idle_seconds: u64,
    pub process_resident_bytes: Option<u64>,
    pub unloaded: bool,
}

/// Retains one CPU engine and its cross-process lease for a single root cache.
#[derive(Default)]
pub struct SemanticSession {
    // Declaration order releases model memory before releasing the inference lock.
    engine: Option<SemanticEngine>,
    lease: Option<InferenceLease>,
    last_used: Option<Instant>,
    last_sample: Option<Instant>,
    pending_activity: bool,
    initializations: u64,
}

impl SemanticSession {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_loaded(&self) -> bool {
        self.engine.is_some()
    }

    pub fn initializations(&self) -> u64 {
        self.initializations
    }

    /// Marks completed semantic work, including errors, before the next request.
    pub fn request_completed(&mut self) {
        self.request_completed_at(Instant::now());
    }

    fn request_completed_at(&mut self, now: Instant) {
        if self.pending_activity {
            self.last_used = Some(now);
            self.pending_activity = false;
        }
    }

    /// Called only between requests; model destruction precedes lease release.
    pub fn idle_tick(&mut self) -> Option<ResidencySample> {
        self.idle_tick_at(Instant::now())
    }

    fn idle_tick_at(&mut self, now: Instant) -> Option<ResidencySample> {
        let last_used = self.last_used?;
        if self.lease.is_none() {
            return None;
        }
        let idle = now.saturating_duration_since(last_used);
        let unloaded = idle >= IDLE_UNLOAD;
        if unloaded {
            self.engine = None;
            self.lease = None;
            self.last_used = None;
        } else if self
            .last_sample
            .is_some_and(|sample| now.saturating_duration_since(sample) < RESIDENCY_INTERVAL)
        {
            return None;
        }
        self.last_sample = Some(now);
        Some(ResidencySample {
            loaded: self.is_loaded(),
            idle_seconds: idle.as_secs(),
            process_resident_bytes: resident_bytes(),
            unloaded,
        })
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
            self.initializations += 1;
            self.last_sample = None;
        }
        self.pending_activity = true;
        self.last_used = Some(Instant::now());
        let engine = self
            .engine
            .as_mut()
            .context("semantic engine initialization failed")?;
        let query = engine.embed(text)?;
        Ok(Some((engine, query)))
    }
}

fn resident_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|line| {
        line.strip_prefix("VmRSS:")?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()?
            .checked_mul(1024)
    })
}

struct InferenceLease {
    cache: PathBuf,
    lock: File,
}

impl Drop for InferenceLease {
    fn drop(&mut self) {
        // A duplicated or inherited descriptor must not retain the released lease.
        let _ = FileExt::unlock(&self.lock);
    }
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
        Ok(Self { cache, lock })
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
    fn semantic_idle_tick_releases_lease_and_throttles_samples() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let now = Instant::now();
        let mut session = SemanticSession {
            lease: Some(InferenceLease::acquire(directory.path())?),
            last_used: Some(now),
            ..SemanticSession::default()
        };
        #[cfg(not(feature = "semantic"))]
        {
            session.engine = Some(SemanticEngine);
            assert!(session.is_loaded());
        }
        assert!(!session.idle_tick_at(now).unwrap().unloaded);
        assert!(
            session
                .idle_tick_at(now + Duration::from_secs(29))
                .is_none()
        );
        assert!(session.idle_tick_at(now + RESIDENCY_INTERVAL).is_some());
        assert!(InferenceLease::acquire(directory.path()).is_err());
        assert!(session.idle_tick_at(now + IDLE_UNLOAD).unwrap().unloaded);
        assert!(!session.is_loaded());
        assert!(InferenceLease::acquire(directory.path()).is_ok());
        assert!(session.idle_tick_at(now + IDLE_UNLOAD).is_none());
        Ok(())
    }

    #[test]
    fn semantic_activity_expires_after_queued_nonsemantic_requests() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let now = Instant::now();
        let mut session = SemanticSession {
            lease: Some(InferenceLease::acquire(directory.path())?),
            last_used: Some(now),
            pending_activity: true,
            ..SemanticSession::default()
        };
        session.request_completed_at(now + Duration::from_secs(1));
        // Ordinary requests do not postpone the last completed semantic work.
        session.request_completed_at(now + IDLE_UNLOAD);
        assert!(
            session
                .idle_tick_at(now + IDLE_UNLOAD + Duration::from_secs(1))
                .unwrap()
                .unloaded
        );
        Ok(())
    }

    #[test]
    fn semantic_inference_lease_releases_duplicated_descriptor() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let lease = InferenceLease::acquire(directory.path())?;
        let inherited = lease.lock.try_clone()?;
        let error = InferenceLease::acquire(directory.path())
            .err()
            .context("second lease succeeded")?;
        assert!(error.to_string().starts_with("semantic_busy:"));
        drop(lease);
        assert!(InferenceLease::acquire(directory.path()).is_ok());
        drop(inherited);
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
