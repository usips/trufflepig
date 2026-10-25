//! Supervision of the per-user router: restart onto a replaced binary.
//! A systemd-supervised router whose executable path changes waits for the new
//! file to settle, drains, and exits with [`ROUTER_RESTART_EXIT_STATUS`] so the
//! unit starts the new binary. Detached routers keep refusing spawns instead.
//! Contract: `docs/router-lifecycle.md`.

mod binary_settle;
mod restart_guard;
#[cfg(test)]
mod tests;

use binary_settle::{BinarySample, FileIdentity, SettleTracker};
use restart_guard::{GuardVerdict, RESTART_LIMIT, RESTART_RECORD, RESTART_WINDOW, RestartGuard};
use std::{
    ffi::OsString,
    fmt,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{Mutex, mpsc},
    time::{Duration, Instant, SystemTime},
};

/// Exit status of a router leaving for its replaced binary (`EX_TEMPFAIL`).
/// The unit lists it in `RestartForceExitStatus=`.
pub(crate) const ROUTER_RESTART_EXIT_STATUS: i32 = 75;
/// Set to `systemd` by the router unit; never inherited by spawned daemons.
pub(crate) const SUPERVISOR_ENV: &str = "TRUFFLEPIG_ROUTER_SUPERVISOR";
/// How long a replacement must stay unchanged before the router exits.
const SETTLE: Duration = Duration::from_secs(30);
const CHECK_INTERVAL: Duration = Duration::from_secs(1);
/// How long an exiting router waits for the daemons it spawned to stop.
pub(super) const SPAWNED_STOP_WAIT: Duration = Duration::from_secs(5);
const MAX_TRACKED_DAEMONS: usize = 256;
const DELETED_SUFFIX: &[u8] = b" (deleted)";

/// Whether systemd runs this router: the unit's explicit marker plus the
/// `INVOCATION_ID` systemd sets for every service invocation.
pub(crate) fn supervised_by_systemd(lookup: impl Fn(&str) -> Option<OsString>) -> bool {
    lookup(SUPERVISOR_ENV).is_some_and(|value| value == "systemd")
        && lookup("INVOCATION_ID").is_some_and(|value| !value.is_empty())
}

/// The router stops serving so its unit restarts it on the replaced binary.
#[derive(Debug)]
pub(crate) struct RouterRestart {
    executable: PathBuf,
    settled: Duration,
}

impl fmt::Display for RouterRestart {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "router_restart: executable {} was replaced and stable for {} s; drained requests and exiting with status {ROUTER_RESTART_EXIT_STATUS} so systemd starts the new binary",
            self.executable.display(),
            self.settled.as_secs()
        )
    }
}

impl std::error::Error for RouterRestart {}

/// The process exit status for a router error: dedicated for a requested restart.
pub(crate) fn exit_status(error: &anyhow::Error) -> Option<i32> {
    error
        .chain()
        .any(|cause| cause.is::<RouterRestart>())
        .then_some(ROUTER_RESTART_EXIT_STATUS)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RestartPhase {
    Current,
    Settling,
    Suppressed { recent: usize },
    Exiting,
}

/// Watches the executable path of a supervised router.
#[derive(Debug)]
struct BinaryWatch {
    executable: PathBuf,
    running: FileIdentity,
    settle_time: Duration,
    settle: SettleTracker,
    guard: RestartGuard,
    next_check: Option<Instant>,
    phase: RestartPhase,
}

impl BinaryWatch {
    /// Samples at most once per [`CHECK_INTERVAL`]; returns a restart once.
    fn tick(&mut self, now: Instant, wall: SystemTime) -> Option<RouterRestart> {
        if self.phase == RestartPhase::Exiting || self.next_check.is_some_and(|next| now < next) {
            return None;
        }
        self.next_check = Some(now + CHECK_INTERVAL);
        let sample = BinarySample::of(&self.executable);
        if sample
            .as_ref()
            .is_some_and(|sample| sample.identity == self.running)
        {
            self.settle.reset();
            self.phase = RestartPhase::Current;
            return None;
        }
        if !self.settle.observe(sample.as_ref(), now, self.settle_time) {
            if !matches!(self.phase, RestartPhase::Suppressed { .. }) {
                self.phase = RestartPhase::Settling;
            }
            return None;
        }
        match self.guard.admit(wall) {
            GuardVerdict::Admitted => {
                self.phase = RestartPhase::Exiting;
                let restart = RouterRestart {
                    executable: self.executable.clone(),
                    settled: self.settle_time,
                };
                eprintln!("trufflepig: {restart}");
                Some(restart)
            }
            verdict => {
                if !matches!(self.phase, RestartPhase::Suppressed { .. }) {
                    eprintln!("trufflepig: {}", self.suppression(&verdict));
                }
                self.phase = RestartPhase::Suppressed {
                    recent: self.guard.recent(wall).len(),
                };
                None
            }
        }
    }

    fn suppression(&self, verdict: &GuardVerdict) -> String {
        let cause = match verdict {
            GuardVerdict::Suppressed {
                recent,
                retry_after,
            } => format!(
                "{recent} automatic restarts in the last {} s (limit {RESTART_LIMIT}); next attempt after {} s",
                RESTART_WINDOW.as_secs(),
                retry_after.as_secs()
            ),
            GuardVerdict::Unrecorded(reason) => reason.clone(),
            GuardVerdict::Admitted => String::new(),
        };
        format!(
            "router_binary_replaced: automatic restart onto {} suppressed: {cause}; spawns keep refusing until `systemctl --user restart trufflepig-system.service`",
            self.executable.display()
        )
    }
}

/// Router supervision state shared by the maintenance thread, request
/// workers that spawn daemons, and the exit path.
#[derive(Debug, Default)]
pub(crate) struct RouterLifecycle {
    watch: Option<Mutex<BinaryWatch>>,
    spawned: Mutex<Vec<PathBuf>>,
}

impl RouterLifecycle {
    /// Supervision for this process: inert unless systemd runs the router.
    pub(crate) fn from_environment(runtime: Option<&Path>) -> Self {
        if !supervised_by_systemd(|name| std::env::var_os(name)) {
            return Self::default();
        }
        let watch = runtime.and_then(|runtime| {
            let executable = std::env::current_exe().ok()?;
            let running = FileIdentity::of(Path::new("/proc/self/exe"))?;
            Some(Mutex::new(BinaryWatch {
                executable: strip_deleted_suffix(executable),
                running,
                settle_time: settle_time(),
                settle: SettleTracker::default(),
                guard: RestartGuard::new(runtime.join(RESTART_RECORD)),
                next_check: None,
                phase: RestartPhase::Current,
            }))
        });
        Self {
            watch,
            spawned: Mutex::default(),
        }
    }

    /// Checks the executable; returns the restart error once the router should exit.
    pub(crate) fn tick(&self, now: Instant, wall: SystemTime) -> Option<RouterRestart> {
        self.watch.as_ref()?.lock().ok()?.tick(now, wall)
    }

    /// Prepares a daemon spawn: no supervisor marker.
    pub(crate) fn prepare_child(&self, command: &mut Command) {
        command.env_remove(SUPERVISOR_ENV);
    }

    /// Remembers a daemon this router started, so a restart can stop it.
    pub(crate) fn record_spawn(&self, cache: &Path) {
        if let Ok(mut spawned) = self.spawned.lock()
            && spawned.len() < MAX_TRACKED_DAEMONS
            && !spawned.iter().any(|known| known == cache)
        {
            spawned.push(cache.to_owned());
        }
    }

    /// Fields for `system status`.
    pub(crate) fn status(&self) -> serde_json::Value {
        let Some(watch) = self.watch.as_ref().and_then(|watch| watch.lock().ok()) else {
            return serde_json::json!({ "supervisor": "detached" });
        };
        let (phase, recent) = match watch.phase {
            RestartPhase::Current => ("current", None),
            RestartPhase::Settling => ("settling", None),
            RestartPhase::Suppressed { recent } => ("suppressed", Some(recent)),
            RestartPhase::Exiting => ("exiting", None),
        };
        serde_json::json!({
            "supervisor": "systemd",
            "binary_restart": phase,
            "recent_restarts": recent,
        })
    }

    /// Asks each spawned daemon to stop and waits up to `wait` for their sockets to close.
    pub(crate) fn stop_spawned(&self, wait: Duration) {
        let caches = self
            .spawned
            .lock()
            .map(|mut spawned| std::mem::take(&mut *spawned))
            .unwrap_or_default();
        let deadline = Instant::now() + wait;
        let (done, finished) = mpsc::channel();
        for cache in caches.iter().cloned() {
            let done = done.clone();
            std::thread::spawn(move || {
                if tp_core::daemon_transport::running(&cache) {
                    let _ = tp_core::daemon_transport::stop(&cache);
                    while tp_core::daemon_transport::running(&cache) && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
                let _ = done.send(());
            });
        }
        drop(done);
        for _ in &caches {
            if finished
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .is_err()
            {
                break;
            }
        }
    }
}

fn strip_deleted_suffix(executable: PathBuf) -> PathBuf {
    let bytes = executable.as_os_str().as_bytes();
    match bytes.strip_suffix(DELETED_SUFFIX) {
        Some(stripped) => PathBuf::from(std::ffi::OsStr::from_bytes(stripped)),
        None => executable,
    }
}

/// [`SETTLE`]; debug builds accept `TRUFFLEPIG_BINARY_SETTLE_MS` for process tests.
fn settle_time() -> Duration {
    #[cfg(debug_assertions)]
    if let Some(millis) = std::env::var("TRUFFLEPIG_BINARY_SETTLE_MS")
        .ok()
        .and_then(|value| value.parse().ok())
    {
        return Duration::from_millis(millis);
    }
    SETTLE
}
