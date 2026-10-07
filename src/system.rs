//! Per-user system daemon routing CLI requests to owning workspace or root daemons.
//! Each request is proxied on its own worker, bounded by `daemon::PROXY_REPLY_WAIT`.

mod board_runtime;
mod route;
pub mod sweep;

pub(crate) use board_runtime::{
    board_router_recently_unavailable, clear_board_router_unavailable,
    mark_board_router_unavailable, record_board_database, validate_board_database,
};
#[cfg(test)]
mod tests;

use crate::{
    background_process::spawn_background,
    daemon::{self, AcceptedRequest, DaemonHandler, deadline::QueryDeadline},
    diagnostics::RequestContext,
};
use anyhow::{Context, Result, bail, ensure};
use route::route;
use std::{
    ffi::OsString,
    fs,
    io::ErrorKind,
    path::PathBuf,
    process::Command,
    sync::Mutex,
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime},
};
use sweep::{SWEEP_GRACE, SWEEP_INTERVAL, sweep};

fn dir_from(
    get: impl Fn(&str) -> Option<OsString>,
    login_session: impl Fn() -> Option<PathBuf>,
) -> Option<PathBuf> {
    // Empty and relative values fall through at every step, including an
    // explicit but empty override: only absolute paths resolve.
    let absolute = |name: &str| {
        get(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    if let Some(dir) = absolute("TRUFFLEPIG_SYSTEM_DIR") {
        return Some(dir);
    }
    if let Some(base) = absolute("XDG_RUNTIME_DIR") {
        return Some(base.join("trufflepig").join("system"));
    }
    if let Some(session) = login_session() {
        return Some(session.join("trufflepig").join("system"));
    }
    let base =
        absolute("XDG_CACHE_HOME").or_else(|| absolute("HOME").map(|home| home.join(".cache")))?;
    Some(base.join("trufflepig").join("system"))
}

/// The login session's `/run/user/<uid>` when it exists, is owned by us,
/// and is mode 0700.
fn login_session_runtime_dir() -> Option<PathBuf> {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let candidate = PathBuf::from(format!("/run/user/{uid}"));
    let metadata = candidate.metadata().ok()?;
    use std::os::unix::fs::MetadataExt;
    (metadata.is_dir() && metadata.uid() == uid && metadata.mode() & 0o7777 == 0o700)
        .then_some(candidate)
}

/// Per-user runtime directory holding the system daemon socket.
pub fn dir() -> Option<PathBuf> {
    dir_from(|name| std::env::var_os(name), login_session_runtime_dir)
}

fn spool_dir_from(get: impl Fn(&str) -> Option<OsString>, uid: u32) -> PathBuf {
    match get("TRUFFLEPIG_SPOOL_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(format!("/tmp/trufflepig-{uid}/spool")),
    }
}

/// Spool directory for sandboxed clients; `/tmp` is what such sandboxes leave writable.
pub fn spool_dir() -> PathBuf {
    // SAFETY: getuid has no preconditions and cannot fail.
    spool_dir_from(|name| std::env::var_os(name), unsafe { libc::getuid() })
}

/// Stops a listening system daemon through its socket.
pub fn stop() -> Result<String> {
    daemon::stop(&dir().context("system_unavailable: no runtime dir")?)
}

/// Sends a request to the system daemon over its socket, else its spool;
/// `None` means no router answered either way.
pub fn request(args: &[String], context: &RequestContext) -> Result<Option<String>> {
    if let Some(dir) = dir()
        && let Some(reply) = daemon::request(&dir, args, context)?
    {
        return Ok(Some(reply));
    }
    daemon::spool::request(
        &spool_dir(),
        args,
        context,
        QueryDeadline::after(Duration::from_secs(30)),
    )
}

/// Longest `ensure` waits for a spawned router's first status answer.
const ROUTER_START_WAIT: Duration = Duration::from_secs(120);

/// Starts the system daemon when no router answers its status ping, then waits
/// up to [`ROUTER_START_WAIT`] for that ping to be answered: a starting router
/// binds its socket before it migrates, so the wait covers migration. Board
/// configuration resolves lazily inside the spawned router, never here.
pub fn ensure() -> Result<()> {
    ensure_within(ROUTER_START_WAIT)
}

/// `ensure` with the router start wait capped at `start_wait`, so tests can
/// inject a short cap and a hung spawn fails fast.
fn ensure_within(start_wait: Duration) -> Result<()> {
    let deadline = QueryDeadline::after(start_wait);
    let ping = vec!["system".to_owned(), "status".to_owned()];
    let context = RequestContext::new(None, None);
    if poll(&ping, &context, deadline)?.is_some() {
        return Ok(());
    }
    ensure_router_starting(deadline, start_wait)?;
    let dir = dir().context("system_unavailable: no runtime dir")?;
    fs::create_dir_all(&dir)?;
    ensure_router_starting(deadline, start_wait)?;
    let mut command = Command::new(std::env::current_exe()?);
    ensure_router_starting(deadline, start_wait)?;
    let child = spawn_background(command.arg("system-serve"))?;
    loop {
        ensure_router_starting(deadline, start_wait)?;
        if poll(&ping, &context, deadline)?.is_some() {
            return Ok(());
        }
        ensure_router_starting(deadline, start_wait)?;
        // A racing spawn exits on the socket bind while its winner serves; a
        // spawn that is gone with no listener fails before the cap.
        if daemon::spawn_failed(&child, &dir) {
            bail!("system_unavailable: daemon did not start");
        }
        std::thread::sleep(Duration::from_millis(25).min(deadline.remaining()));
    }
}

/// Reports the existing startup failure after the one absolute deadline expires.
fn ensure_router_starting(deadline: QueryDeadline, start_wait: Duration) -> Result<()> {
    ensure!(
        !deadline.expired(),
        "system_unavailable: router still starting after {} s",
        start_wait.as_secs()
    );
    Ok(())
}

/// Probes the socket and spool using the same absolute startup deadline.
fn poll(
    ping: &[String],
    context: &RequestContext,
    deadline: QueryDeadline,
) -> Result<Option<String>> {
    if deadline.expired() {
        return Ok(None);
    }
    let Some(dir) = dir() else {
        return Ok(None);
    };
    if deadline.expired() {
        return Ok(None);
    }
    let reply = match daemon::request_by(&dir, ping, context, deadline) {
        Ok(reply) => reply,
        Err(error) if poll_budget_spent(&error) => None,
        Err(error) => return Err(error),
    };
    if let Some(reply) = reply {
        return Ok((!deadline.expired()).then_some(reply));
    }
    if deadline.expired() {
        return Ok(None);
    }
    match daemon::spool::request(&spool_dir(), ping, context, deadline) {
        Ok(_) if deadline.expired() => Ok(None),
        Ok(reply) => Ok(reply),
        Err(error) if poll_budget_spent(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Whether a bounded poll error only reports its own expired budget.
fn poll_budget_spent(error: &anyhow::Error) -> bool {
    daemon::deadline::is_timed_out(error)
        || error.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<std::io::Error>(),
                Some(io) if matches!(io.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
            )
        })
}

/// Serves the system daemon, proxying socket and spooled requests to their
/// owners concurrently and sweeping the default cache base at startup and every
/// `SWEEP_INTERVAL` on a background thread, so a slow daemon stop never
/// blocks the request path.
pub fn serve() -> Result<()> {
    let runtime = dir().context("system_unavailable: no runtime dir")?;
    let router = SystemRouter {
        runtime: Some(runtime.clone()),
        cache_base: crate::cli::cache_base().ok(),
        sweeps: Mutex::new(SweepClock::default()),
        board: crate::board::BoardHost::default(),
    };
    // Migration runs in `on_bound`, after the socket binds and before the
    // accept loop, so waiting clients queue instead of spawning a second router.
    daemon::serve_router(&runtime, &spool_dir(), router)
}

/// The router's request handler; each request is routed independently.
struct SystemRouter {
    runtime: Option<PathBuf>,
    cache_base: Option<PathBuf>,
    sweeps: Mutex<SweepClock>,
    board: crate::board::BoardHost,
}

#[derive(Default)]
struct SweepClock {
    last_sweep: Option<Instant>,
    running: Option<JoinHandle<()>>,
}

impl DaemonHandler for SystemRouter {
    fn on_bound(&self) {
        // Best-effort: the router must serve despite a broken board configuration.
        let _ = self
            .board
            .ensure_writer(QueryDeadline::after(Duration::from_secs(5)));
    }

    fn request(&self, request: AcceptedRequest) -> Result<String> {
        route(
            &self.board,
            self.runtime.as_deref(),
            request.args,
            request.context,
            request.deadline,
        )
    }

    fn idle(&self) {
        self.board.idle();
        let (Some(base), Ok(mut clock)) = (&self.cache_base, self.sweeps.lock()) else {
            return;
        };
        if clock.running.as_ref().is_none_or(JoinHandle::is_finished)
            && clock
                .last_sweep
                .is_none_or(|started| started.elapsed() >= SWEEP_INTERVAL)
        {
            if let Some(finished) = clock.running.take() {
                let _ = finished.join();
            }
            let base = base.clone();
            clock.running = Some(std::thread::spawn(move || {
                sweep(&base, SystemTime::now(), SWEEP_GRACE);
            }));
            clock.last_sweep = Some(Instant::now());
        }
    }
}
