//! Per-user system daemon routing CLI requests to owning workspace or root daemons.
//! Each request is proxied on its own worker, bounded by `daemon::PROXY_REPLY_WAIT`.

mod board_runtime;
pub mod sweep;

pub(crate) use board_runtime::{
    board_router_recently_unavailable, clear_board_router_unavailable,
    mark_board_router_unavailable, record_board_database, validate_board_database,
};
#[cfg(test)]
mod tests;

use crate::{
    background_process::{BackgroundChild, spawn_background},
    cli::normalized_args,
    daemon::{self, AcceptedRequest, DaemonHandler, deadline::QueryDeadline},
    diagnostics::RequestContext,
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime},
};
use sweep::{SWEEP_GRACE, SWEEP_INTERVAL, sweep};

fn dir_from(get: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    if let Some(dir) = get("TRUFFLEPIG_SYSTEM_DIR") {
        return Some(PathBuf::from(dir));
    }
    let base = get("XDG_RUNTIME_DIR")
        .or_else(|| get("XDG_CACHE_HOME"))
        .map(PathBuf::from)
        .or_else(|| get("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(base.join("trufflepig").join("system"))
}

/// Per-user runtime directory holding the system daemon socket.
pub fn dir() -> Option<PathBuf> {
    dir_from(|name| std::env::var_os(name))
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
    daemon::spool::request(&spool_dir(), args, context)
}

/// Starts the system daemon when no router answers its status ping.
pub fn ensure() -> Result<()> {
    let ping = vec!["system".to_owned(), "status".to_owned()];
    let context = RequestContext::new(None, None);
    if request(&ping, &context)?.is_some() {
        return Ok(());
    }
    let dir = dir().context("system_unavailable: no runtime dir")?;
    fs::create_dir_all(&dir)?;
    let mut command = Command::new(std::env::current_exe()?);
    command.env(
        "TRUFFLEPIG_BOARD_DB",
        crate::board::BoardConfig::database_path()?,
    );
    spawn_background(command.arg("system-serve"))?;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if request(&ping, &context)?.is_some() {
            return Ok(());
        }
        // A racing spawn exits on the socket bind while its winner serves.
        std::thread::sleep(Duration::from_millis(25));
    }
    bail!("system_unavailable: daemon did not start")
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

fn route(
    board: &crate::board::BoardHost,
    runtime: Option<&Path>,
    args: Vec<String>,
    context: RequestContext,
    deadline: QueryDeadline,
) -> Result<String> {
    // Preserve time spent queued at this router, and cap every forwarded wait.
    let forwarding = deadline.capped(daemon::PROXY_REPLY_WAIT);
    let options = crate::cli::parse(&args)?;
    let verb = options.words.first().map(String::as_str);
    if verb == Some("system") {
        let database = board.database_path()?;
        if let Some(runtime) = runtime {
            record_board_database(runtime, &database)?;
        }
        let mut status = serde_json::json!({
            "status": "ok", "board_api": crate::board::BOARD_API, "board_db": database,
        });
        if let Some(version) = board_schema_version(&database) {
            status["schema_version"] = version.into();
        }
        return Ok(serde_json::to_string(&status)?);
    }
    if matches!(verb, Some("board" | "feedback")) {
        return board.run(&options, &context, deadline);
    }
    if let Some(config) = crate::workspace::resolve(&options)? {
        let cache = crate::workspace::cache_path(&config, options.cache.as_deref())?;
        if verb == Some("stop") {
            refuse_router_stop(&cache)?;
            return daemon::stop(&cache);
        }
        if let Some(reply) = daemon::request_by(&cache, &args, &context, forwarding)? {
            return Ok(reply);
        }
        let mut server = options.clone();
        server.words = vec!["workspace-serve".into()];
        server.member = None;
        let mut command = Command::new(std::env::current_exe()?);
        command.args(normalized_args(&server, &server.root));
        let child = spawn_background(&mut command)?;
        return forward_spawned(&cache, &args, &context, &child, forwarding);
    }
    let root = options
        .root
        .canonicalize()
        .context("invalid_root: cannot open repository root")?;
    let cache = crate::cli::cache_path(&root, options.cache.as_deref())?;
    if verb == Some("stop") {
        refuse_router_stop(&cache)?;
        return daemon::stop(&cache);
    }
    if let Some(reply) = daemon::request_by(&cache, &args, &context, forwarding)? {
        return Ok(reply);
    }
    fs::create_dir_all(&cache)?;
    let history_cache = options.resolved_history_cache.clone().or_else(|| {
        crate::history::worker::resolve_cache(
            &root,
            options.cache.as_deref(),
            options.history_cache.as_deref(),
        )
        .ok()
    });
    let mut command = Command::new(std::env::current_exe()?);
    if let Some(history_cache) = history_cache {
        command.arg("--resolved-history-cache").arg(history_cache);
    }
    command
        .arg("--no-workspace")
        .arg("--diagnostics")
        .arg(&options.diagnostics);
    let child = spawn_background(
        command
            .arg("--root")
            .arg(&root)
            .arg("--cache")
            .arg(&cache)
            .arg("serve"),
    )?;
    forward_spawned(&cache, &args, &context, &child, forwarding)
}

/// Best-effort read of the board database's storage schema; absent or
/// unreadable storage reports no version instead of breaking status.
fn board_schema_version(database: &Path) -> Option<i64> {
    let connection = rusqlite::Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .ok()
}

/// Refuses a stop aimed at this router's own socket directory.
fn refuse_router_stop(cache: &Path) -> Result<()> {
    if let Some(dir) = dir() {
        ensure!(cache != dir, "invalid_cache: cannot stop the system router");
    }
    Ok(())
}

/// Polls a spawned daemon with the real args, returning its first reply.
fn forward_spawned(
    cache: &Path,
    args: &[String],
    context: &RequestContext,
    child: &BackgroundChild,
    forwarding: QueryDeadline,
) -> Result<String> {
    let deadline = Instant::now() + forwarding.cap(Duration::from_secs(3));
    while Instant::now() < deadline {
        if let Some(reply) = daemon::request_by(cache, args, context, forwarding)? {
            return Ok(reply);
        }
        if daemon::spawn_failed(child, cache) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    bail!("daemon_unavailable: target did not start")
}
