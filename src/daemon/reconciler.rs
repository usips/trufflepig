//! The maintenance thread: watches the root, schedules reconciliation, detects a
//! vanished root, drains the spool, and runs idle work. Requests never wait on it.
use super::pool::RequestPool;
use super::server::{DaemonHandler, ServerState};
use super::spool::SpoolServer;
use notify::{EventKind, RecursiveMode, Watcher};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Periodic recovery reconcile without a watcher (polling mode).
pub(super) const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);
/// Periodic recovery reconcile while a watcher delivers change hints. Walking a
/// large unchanged tree every 30 s kept idle daemons at a fifth of a core each.
pub(super) const WATCHED_RECONCILE_INTERVAL: Duration = Duration::from_secs(300);
/// Build and VCS directories the index walk prunes; their churn is not a change.
const UNINDEXED_DIRECTORIES: [&str; 4] = [".git", "target", "node_modules", ".trufflepig"];
pub(super) const DEBOUNCE: Duration = Duration::from_millis(75);
pub(super) const MAX_DEBOUNCE: Duration = Duration::from_secs(1);
/// Pause between maintenance ticks (schedule check, spool drain, idle work).
const MAINTENANCE_TICK: Duration = Duration::from_millis(20);

pub(super) struct ReconcileSchedule {
    interval: Duration,
    last_reconcile: Instant,
    first_event: Option<Instant>,
    latest_event: Option<Instant>,
}

impl ReconcileSchedule {
    pub(super) fn new(now: Instant, interval: Duration) -> Self {
        Self {
            interval,
            last_reconcile: now,
            first_event: None,
            latest_event: None,
        }
    }

    pub(super) fn due(&mut self, now: Instant, changed: bool) -> bool {
        if changed {
            self.first_event.get_or_insert(now);
            self.latest_event = Some(now);
        }
        now.duration_since(self.last_reconcile) >= self.interval
            || self
                .latest_event
                .is_some_and(|time| now.duration_since(time) >= DEBOUNCE)
            || self
                .first_event
                .is_some_and(|time| now.duration_since(time) >= MAX_DEBOUNCE)
    }

    fn completed(&mut self) {
        self.last_reconcile = Instant::now();
        self.first_event = None;
        self.latest_event = None;
    }
}

/// Whether a watched path may affect the index: outside pruned build/VCS trees.
pub(super) fn indexed_path(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root).map_or(true, |relative| {
        !relative.components().any(|part| {
            UNINDEXED_DIRECTORIES
                .iter()
                .any(|name| part.as_os_str() == *name)
        })
    })
}

/// Lowers the calling thread (and threads it spawns later) to nice 10 and idle
/// I/O; on Linux both apply per thread, so request workers keep normal priority.
fn lower_priority() {
    // SAFETY: plain syscalls on the calling thread with constant arguments.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
        // IOPRIO_WHO_PROCESS = 1; IOPRIO_CLASS_IDLE = 3 in the top three bits.
        libc::syscall(libc::SYS_ioprio_set, 1, 0, 3 << 13);
    }
}

fn watch(root: &Path, cache: &Path, dirty: Arc<AtomicBool>) -> Option<notify::RecommendedWatcher> {
    let cache = cache.to_owned();
    let watched_root = root.to_owned();
    let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let changed = match event {
            Ok(event) => {
                if event.need_rescan() {
                    eprintln!("trufflepig: watcher lost events; scheduling reconciliation");
                }
                event.need_rescan()
                    || (!matches!(event.kind, EventKind::Access(_))
                        && (event.paths.is_empty()
                            || event.paths.iter().any(|path| {
                                !path.starts_with(&cache) && indexed_path(&watched_root, path)
                            })))
            }
            Err(error) => {
                eprintln!("trufflepig: watcher failed; scheduling reconciliation: {error}");
                true
            }
        };
        if changed {
            dirty.store(true, Ordering::Release);
        }
    });
    match watcher.and_then(|mut watcher| {
        watcher.watch(root, RecursiveMode::Recursive)?;
        Ok(watcher)
    }) {
        Ok(watcher) => Some(watcher),
        Err(error) => {
            eprintln!("trufflepig: watcher unavailable; using periodic reconciliation: {error}");
            None
        }
    }
}

/// What the maintenance thread owns besides the handler.
pub(super) struct Maintenance {
    pub root: PathBuf,
    pub cache: PathBuf,
    pub watching: bool,
    pub spool: Option<SpoolServer>,
}

/// Runs until the server stops. A failed initial reconcile or a vanished root
/// stops the server; later reconcile failures keep periodic retry active.
pub(super) fn run<H: DaemonHandler>(
    mut maintenance: Maintenance,
    handler: &Arc<H>,
    pool: &Arc<RequestPool>,
    state: &ServerState,
) {
    let dirty = Arc::new(AtomicBool::new(false));
    let watcher = if maintenance.watching {
        watch(&maintenance.root, &maintenance.cache, Arc::clone(&dirty))
    } else {
        None
    };
    // The initial scan runs at normal priority: it is what a fresh root's first
    // queries wait for. Later reconciles yield to interactive work.
    let Some(initial) = survive(state, || handler.reconcile()) else {
        return;
    };
    if let Err(error) = initial {
        state.fail(error.context("initial repository reconciliation"));
        return;
    }
    if maintenance.watching {
        lower_priority();
    }
    let interval = if watcher.is_some() {
        WATCHED_RECONCILE_INTERVAL
    } else {
        RECONCILE_INTERVAL
    };
    let mut schedule = ReconcileSchedule::new(Instant::now(), interval);
    while !state.stopping() {
        if schedule.due(Instant::now(), dirty.swap(false, Ordering::AcqRel)) {
            if maintenance.watching && !maintenance.root.is_dir() {
                eprintln!(
                    "trufflepig: root {} is gone; daemon exiting",
                    maintenance.root.display()
                );
                state.stop();
                return;
            }
            let Some(reconciled) = survive(state, || handler.reconcile()) else {
                return;
            };
            if let Err(error) = reconciled {
                eprintln!(
                    "trufflepig: reconciliation failed; periodic retry remains active: {error:#}"
                );
            }
            schedule.completed();
        }
        if let Some(spool) = &mut maintenance.spool {
            super::server::dispatch_spooled(spool, handler, pool);
        }
        if survive(state, || handler.idle()).is_none() {
            return;
        }
        std::thread::sleep(MAINTENANCE_TICK);
    }
}

/// Runs maintenance work; a panic stops the server with an `internal_error`
/// (the next client starts a fresh daemon) instead of leaving it unmaintained.
fn survive<T>(state: &ServerState, work: impl FnOnce() -> T) -> Option<T> {
    match catch_unwind(AssertUnwindSafe(work)) {
        Ok(value) => Some(value),
        Err(panic) => {
            state.fail(anyhow::anyhow!(
                "internal_error: daemon maintenance panicked: {}",
                super::server::panic_message(panic.as_ref())
            ));
            None
        }
    }
}
