//! Per-user system daemon routing CLI requests to owning workspace or root daemons.
//! Each request is proxied on its own worker, bounded by `crate::daemon_transport::PROXY_REPLY_WAIT`.

mod managed_unit;
mod route;
pub mod sweep;

use crate::coded_error::CodedContext;
use crate::runtime_paths::{spool_dir, system_dir};
#[cfg(test)]
mod tests;

use crate::{
    background_process::{
        BackgroundChild, BackgroundOwner, current_background_executable, spawn_background_for,
    },
    daemon::{self, AcceptedRequest, DaemonHandler},
    query_deadline::QueryDeadline,
    request_context::RequestContext,
};
use anyhow::{Result, bail};
use route::route;
use std::{
    fs,
    io::ErrorKind,
    path::PathBuf,
    process::Command,
    sync::Mutex,
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime},
};
use sweep::{SWEEP_GRACE, SWEEP_INTERVAL, sweep};

/// Stops a listening system daemon through its socket.
pub fn stop() -> Result<String> {
    crate::daemon_transport::stop(
        &system_dir().with_coded_context(|| coded!(SystemUnavailable, "no runtime dir"))?,
    )
}

/// Longest `ensure` waits for a spawned router's first status answer.
const ROUTER_START_WAIT: Duration = Duration::from_secs(120);
const ROUTER_STATUS_PROBE: Duration = Duration::from_millis(250);

/// Starts the system daemon when no router answers its status ping, then waits
/// up to [`ROUTER_START_WAIT`] for that ping to be answered: a starting router
/// binds its socket before it migrates, so the wait covers migration. Board
/// configuration resolves lazily inside the spawned router, never here.
pub fn ensure() -> Result<()> {
    ensure_by(QueryDeadline::after(ROUTER_START_WAIT))
}

/// Ensures a router under the caller's absolute request deadline.
pub(crate) fn ensure_by(deadline: QueryDeadline) -> Result<()> {
    let start_wait = deadline.cap(ROUTER_START_WAIT);
    ensure_until(deadline.capped(ROUTER_START_WAIT), start_wait, start_router)
}

/// Starts the router through the installed unit when it owns this runtime
/// directory, else as a detached `system-serve`.
fn start_router(deadline: QueryDeadline) -> Result<managed_unit::RouterStart<'static>> {
    let dir = system_dir().with_coded_context(|| coded!(SystemUnavailable, "no runtime dir"))?;
    let unit = managed_unit::installed_unit_text()?;
    let systemctl_deadline = deadline.clone();
    managed_unit::start_router_with(
        unit.as_deref(),
        &dir,
        &deadline,
        move |args| managed_unit::run_systemctl(args, &systemctl_deadline),
        spawn_router,
    )
}

fn spawn_router() -> Result<BackgroundChild> {
    let cache = system_dir().with_coded_context(|| coded!(SystemUnavailable, "no runtime dir"))?;
    let executable = current_background_executable(&cache, BackgroundOwner::Command)?;
    let mut command = Command::new(&executable);
    command.arg("system-serve");
    spawn_background_for(&mut command, &executable, "system router", &cache)
}

/// Starts the router with `start`, then polls its status. A detached child is
/// watched for exit; an installed unit is watched through its systemd state.
fn ensure_until<'run>(
    deadline: QueryDeadline,
    start_wait: Duration,
    start: impl FnOnce(QueryDeadline) -> Result<managed_unit::RouterStart<'run>>,
) -> Result<()> {
    let ping = ["system".to_owned(), "status".to_owned()];
    let context = RequestContext::new(None, None);
    if poll(&ping, &context, deadline.capped(ROUTER_STATUS_PROBE))?.is_some() {
        return Ok(());
    }
    ensure_router_starting(deadline.clone(), start_wait)?;
    let dir = system_dir().with_coded_context(|| coded!(SystemUnavailable, "no runtime dir"))?;
    let started = if crate::daemon_transport::running_by(&dir, &deadline)? {
        None
    } else {
        ensure_router_starting(deadline.clone(), start_wait)?;
        fs::create_dir_all(&dir).with_coded_context(|| {
            coded!(
                RouterCacheCreateFailed,
                "cannot create system router cache {}",
                dir.display()
            )
        })?;
        ensure_router_starting(deadline.clone(), start_wait)?;
        Some(start(deadline.clone())?)
    };
    let (child, mut unit) = match started {
        Some(managed_unit::RouterStart::Child(child)) => (Some(child), None),
        Some(managed_unit::RouterStart::Unit(unit)) => (None, Some(unit)),
        None => (None, None),
    };
    loop {
        ensure_router_starting(deadline.clone(), start_wait)?;
        if poll(&ping, &context, deadline.clone())?.is_some() {
            return Ok(());
        }
        ensure_router_starting(deadline.clone(), start_wait)?;
        // A racing spawn exits on the socket bind while its winner serves; a
        // spawn that is gone with no listener fails before the cap.
        if child.as_ref().is_some_and(BackgroundChild::exited)
            && !crate::daemon_transport::running_by(&dir, &deadline)?
        {
            bail!(coded!(
                SystemUnavailable,
                "system router for cache {} did not start",
                dir.display()
            ));
        }
        // A unit that failed or crash-loops has no child to exit, so its state
        // decides the start; a router that already answers is left to the poll.
        if let Some(state) = unit.as_mut().and_then(|unit| unit.failure(Instant::now()))
            && !crate::daemon_transport::running_by(&dir, &deadline)?
        {
            return Err(managed_unit::did_not_start(&state));
        }
        std::thread::sleep(Duration::from_millis(25).min(deadline.remaining()));
    }
}

/// Reports the existing startup failure after the one absolute deadline expires.
fn ensure_router_starting(deadline: QueryDeadline, start_wait: Duration) -> Result<()> {
    if deadline.expired() {
        let cache = system_dir()
            .map(|cache| cache.display().to_string())
            .unwrap_or_else(|| "<unavailable>".into());
        bail!(coded!(
            SystemUnavailable,
            "router still starting after {} s for cache {cache}",
            start_wait.as_secs()
        ));
    }
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
    poll_at(
        system_dir().as_deref(),
        &spool_dir(),
        ping,
        context,
        deadline,
    )
}

fn poll_at(
    runtime: Option<&std::path::Path>,
    spool: &std::path::Path,
    ping: &[String],
    context: &RequestContext,
    deadline: QueryDeadline,
) -> Result<Option<String>> {
    if deadline.expired() {
        return Ok(None);
    }
    if let Some(dir) = runtime {
        let reply = match crate::daemon_transport::request_by(dir, ping, context, deadline.clone())
        {
            Ok(reply) => reply,
            Err(error) if poll_budget_spent(&error) => None,
            Err(error) => return Err(error),
        };
        if let Some(reply) = reply {
            return Ok((!deadline.expired()).then_some(reply));
        }
    }
    if deadline.expired() {
        return Ok(None);
    }
    match crate::daemon_transport::spool::request(spool, ping, context, deadline.clone()) {
        Ok(_) if deadline.expired() => Ok(None),
        Ok(reply) => Ok(reply),
        Err(error) if poll_budget_spent(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Whether a bounded poll error only reports its own expired budget.
fn poll_budget_spent(error: &anyhow::Error) -> bool {
    crate::daemon_transport::spool::is_spool_or_coded_timeout(error)
        || crate::daemon_transport::DaemonReplyTimeout::from_error(error).is_some()
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
    let runtime =
        system_dir().with_coded_context(|| coded!(SystemUnavailable, "no runtime dir"))?;
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
                let home = std::env::var_os("HOME").map(PathBuf::from);
                sweep(&base, home.as_deref(), SystemTime::now(), SWEEP_GRACE);
            }));
            clock.last_sweep = Some(Instant::now());
        }
    }
}
