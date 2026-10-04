//! Lazy local writer, bounded inbox waiting, and edge maintenance for the router.

mod board_dispatch;
mod board_maintenance;
mod board_wait;
mod board_writer;
#[cfg(test)]
mod tests;

use crate::board::{
    board_config::{BoardConfig, BoardConfigCache},
    board_ids::EventSeq,
    board_protocol::{BoardError, BoardReply, BoardRequest},
    commit_ingest::RepoIngestor,
    feedback_outbox::ImportSummary,
    local_board::LocalBoard,
    repo_identity::RepoIdentityCache,
};
use crate::daemon::deadline::QueryDeadline;
use anyhow::Result;
use board_writer::check_deadline;
pub(crate) use board_writer::ensure_feedback_import;
use std::{
    sync::{Arc, Condvar, Mutex, TryLockError, atomic::AtomicUsize},
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// Typed operations return domain evidence; edge rendering and git stay outside it.
pub trait BoardBackend: Send {
    fn handle(&mut self, request: &BoardRequest) -> std::result::Result<BoardReply, BoardError>;
    fn import_feedback(
        &mut self,
        request: &BoardRequest,
    ) -> std::result::Result<BoardReply, BoardError>;
    fn max_seq(&self) -> std::result::Result<EventSeq, BoardError>;
}

#[derive(Clone, Default)]
pub struct BoardHost {
    inner: Arc<BoardHostShared>,
}

#[derive(Default)]
struct BoardHostShared {
    config: Mutex<BoardConfigCache>,
    backend: Mutex<Option<LocalBoard>>,
    ingestor: Mutex<RepoIngestor>,
    registrations: Mutex<RepoIdentityCache>,
    sequence: Mutex<EventSeq>,
    changed: Condvar,
    waiters: AtomicUsize,
    #[cfg(test)]
    inbox_queries: AtomicUsize,
    maintenance: Mutex<BoardMaintenanceClock>,
}

#[derive(Default)]
struct BoardMaintenanceClock {
    last_started: Option<Instant>,
    running: Option<JoinHandle<std::result::Result<Option<ImportSummary>, BoardError>>>,
    running_import: bool,
    last_checked: Option<Instant>,
    import_retry_at: Option<Instant>,
    import_delay: Duration,
    reported_errors: Vec<&'static str>,
}

impl BoardHost {
    /// Explicit configuration keeps tests and coordinators independent of process env.
    pub fn with_config(config: BoardConfig) -> Self {
        Self {
            inner: Arc::new(BoardHostShared {
                config: Mutex::new(BoardConfigCache::with_config(config)),
                ..BoardHostShared::default()
            }),
        }
    }

    pub(super) fn config(&self) -> Result<BoardConfig> {
        let mut config = self.inner.config.lock().unwrap_or_else(|poisoned| {
            self.inner.config.clear_poison();
            poisoned.into_inner()
        });
        config.get(Instant::now()).map_err(Into::into)
    }

    pub(crate) fn database_path(&self) -> Result<std::path::PathBuf> {
        let mut config = self.inner.config.lock().unwrap_or_else(|poisoned| {
            self.inner.config.clear_poison();
            poisoned.into_inner()
        });
        let _ = config.get(Instant::now());
        config
            .database_path()
            .map(std::path::Path::to_path_buf)
            .ok_or_else(|| anyhow::anyhow!("board_unavailable: database path is unavailable"))
    }

    /// The latest board configuration failure, for `system status` to report
    /// when no database path is available; `None` means unloaded or healthy.
    pub(crate) fn config_error(&self) -> Option<String> {
        let config = self.inner.config.lock().unwrap_or_else(|poisoned| {
            self.inner.config.clear_poison();
            poisoned.into_inner()
        });
        config.snapshot()?.err().map(|error| error.to_string())
    }

    pub(super) fn idle_config(
        &self,
    ) -> Option<(Option<std::result::Result<BoardConfig, BoardError>>, bool)> {
        let config = match self.inner.config.try_lock() {
            Ok(config) => config,
            Err(TryLockError::WouldBlock) => return None,
            Err(TryLockError::Poisoned(poisoned)) => {
                self.inner.config.clear_poison();
                poisoned.into_inner()
            }
        };
        Some((config.snapshot(), config.needs_refresh(Instant::now())))
    }

    pub(super) fn max_seq_by(&self, deadline: QueryDeadline) -> Result<EventSeq> {
        check_deadline(deadline)?;
        let config = self.config()?;
        if !config.db_path.exists() {
            return Ok(EventSeq::new(0));
        }
        let reader =
            LocalBoard::open_read_with_timeout(&config, deadline.cap(Duration::from_millis(100)))?;
        check_deadline(deadline)?;
        reader.max_seq().map_err(Into::into)
    }
}
