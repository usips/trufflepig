//! The per-root index daemon: query workers read the published index while the
//! maintenance thread reconciles, schedules preparation, and runs probes.
use super::{Arguments, dispatch::local_with_session, emission, parse};
use crate::{
    daemon::{self, AcceptedRequest, DaemonHandler},
    diagnostics::DiagnosticQueue,
    semantic::{SemanticSession, preparation::PreparationManager},
    store::Store,
};
use anyhow::Result;
use rusqlite::OptionalExtension;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const PREPARATION_CHECK_INTERVAL: Duration = Duration::from_secs(1);
const PROBE_INTERVAL: Duration = Duration::from_secs(30);

/// Serves `root` from `cache` in the foreground until stopped or the root is gone.
/// The socket binds before any database work: the initial reconcile creates the
/// schema and opens the diagnostics journal while reads answer `index_warming`.
pub(super) fn serve(root: &Path, cache: &Path, options: &Arguments) -> Result<()> {
    let history_cache = options.resolved_history_cache.clone().or_else(|| {
        crate::history::worker::resolve_cache(
            root,
            options.cache.as_deref(),
            options.history_cache.as_deref(),
        )
        .ok()
    });
    let _heartbeat = history_cache
        .as_ref()
        .and_then(|path| crate::history::worker::Heartbeat::start(root, path).ok());
    daemon::serve(root, cache, RootDaemon::open(root, cache, options))
}

/// Per-root daemon state; requests hold no lock across their dispatch.
pub(crate) struct RootDaemon {
    root: PathBuf,
    cache: PathBuf,
    diagnostics: crate::diagnostics::DiagnosticsMode,
    preparation: PreparationManager,
    /// Opened by the first reconcile and replaced after `forget-logs`;
    /// requests record through a cloned handle.
    log_queue: RwLock<Option<Arc<DiagnosticQueue>>>,
    journal_opened: AtomicBool,
    maintenance: Mutex<MaintenanceClock>,
}

struct MaintenanceClock {
    last_probe: Instant,
    last_preparation_check: Instant,
}

impl RootDaemon {
    pub(crate) fn open(root: &Path, cache: &Path, options: &Arguments) -> Self {
        let diagnostics = emission::diagnostics_mode(options);
        Self {
            root: root.to_owned(),
            cache: cache.to_owned(),
            diagnostics,
            preparation: PreparationManager::new(crate::semantic::preparation::SharedWorker::new(
                root,
            )),
            log_queue: RwLock::new(None),
            journal_opened: AtomicBool::new(false),
            maintenance: Mutex::new(MaintenanceClock {
                last_probe: Instant::now(),
                last_preparation_check: Instant::now(),
            }),
        }
    }

    fn log_queue(&self) -> Option<Arc<DiagnosticQueue>> {
        self.log_queue.read().ok().and_then(|queue| queue.clone())
    }

    fn reopen_journal(&self) {
        if let Ok(mut queue) = self.log_queue.write() {
            *queue = DiagnosticQueue::open(&self.cache, self.diagnostics)
                .ok()
                .map(Arc::new);
        }
    }

    fn probe(&self) {
        let Ok(store) = Store::open(&self.root, &self.cache) else {
            return;
        };
        let Ok(report) = crate::probes::doctor(&store, &self.cache, &SemanticSession::default())
        else {
            return;
        };
        let failed = report
            .probes
            .iter()
            .any(|p| matches!(p.outcome, crate::probes::ProbeOutcome::Failed));
        let mut event = crate::diagnostics::RequestEvent::new(
            crate::diagnostics::RequestContext::new(None, None),
            crate::diagnostics::Operation::Doctor,
            if failed {
                crate::diagnostics::Outcome::Failure
            } else {
                crate::diagnostics::Outcome::Success
            },
        );
        event.stage = crate::diagnostics::EventStage::Maintenance;
        event.coverage = Some(report.coverage);
        event.probes = report.probes;
        if let Some(queue) = self.log_queue() {
            queue.record(event);
        }
    }
}

impl DaemonHandler for RootDaemon {
    fn request(&self, request: AcceptedRequest) -> Result<String> {
        let options = parse(&request.args)?;
        let output = local_with_session(
            &self.root,
            &self.cache,
            &options,
            true,
            &mut SemanticSession::default(),
            &request.context,
            self.log_queue().as_deref(),
            Some(&self.preparation),
            request.deadline,
        );
        if output.is_ok()
            && options
                .words
                .first()
                .is_some_and(|verb| verb == "forget-logs")
        {
            self.reopen_journal();
        }
        output
    }

    fn reconcile(&self) -> Result<()> {
        if !self.journal_opened.swap(true, Ordering::AcqRel) {
            self.reopen_journal();
        }
        Store::open(&self.root, &self.cache)?.index()?;
        schedule_pending_preparation(&self.preparation, &self.root, &self.cache)
    }

    fn idle(&self) {
        let Ok(mut clock) = self.maintenance.lock() else {
            return;
        };
        if clock.last_preparation_check.elapsed() >= PREPARATION_CHECK_INTERVAL {
            if let Err(error) =
                schedule_pending_preparation(&self.preparation, &self.root, &self.cache)
            {
                eprintln!("trufflepig: semantic preparation scheduling failed: {error}");
            }
            clock.last_preparation_check = Instant::now();
        }
        if clock.last_probe.elapsed() >= PROBE_INTERVAL {
            self.probe();
            clock.last_probe = Instant::now();
        }
    }
}

/// Re-wakes a manager after indexing when a persisted preparation request was
/// made while no root daemon was serving. The marker keeps preparation opt-in.
pub(super) fn schedule_pending_preparation(
    manager: &PreparationManager,
    root: &Path,
    cache: &Path,
) -> Result<()> {
    let path = cache.join("preparation.sqlite3");
    if !path.is_file() {
        return Ok(());
    }
    let connection = rusqlite::Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(1))?;
    let requested: i64 = match connection.query_row(
        "SELECT requested_generation FROM preparation_requests WHERE id=1",
        [],
        |row| row.get(0),
    ) {
        Ok(requested) => requested,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let generation = Store::open(root, cache)?.generation()?;
    if requested <= 0 || generation <= 0 {
        return Ok(());
    }
    let state: Option<String> = connection
        .query_row(
            "SELECT state FROM preparation_runs WHERE generation=?1",
            [generation],
            |row| row.get(0),
        )
        .optional()?;
    if state
        .as_deref()
        .is_some_and(|state| matches!(state, "completed" | "failed" | "capacity"))
    {
        return Ok(());
    }
    manager.schedule(root, cache)?;
    Ok(())
}

#[cfg(test)]
mod tests;
