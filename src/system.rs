//! Per-user system daemon routing CLI requests to owning workspace or root daemons.
//! Each request is proxied on its own worker, bounded by `daemon::PROXY_REPLY_WAIT`.

pub mod sweep;
#[cfg(test)]
mod tests;

use crate::{
    background_process::{BackgroundChild, spawn_background},
    cli::normalized_args,
    daemon::{self, AcceptedRequest, DaemonHandler},
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
    let router = SystemRouter {
        cache_base: crate::cli::cache_base().ok(),
        sweeps: Mutex::new(SweepClock::default()),
    };
    daemon::serve_router(
        &dir().context("system_unavailable: no runtime dir")?,
        &spool_dir(),
        router,
    )
}

/// The router's request handler; each request is routed independently.
struct SystemRouter {
    cache_base: Option<PathBuf>,
    sweeps: Mutex<SweepClock>,
}

#[derive(Default)]
struct SweepClock {
    last_sweep: Option<Instant>,
    running: Option<JoinHandle<()>>,
}

impl DaemonHandler for SystemRouter {
    fn request(&self, request: AcceptedRequest) -> Result<String> {
        route(request.args, request.context)
    }

    fn idle(&self) {
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

fn route(args: Vec<String>, context: RequestContext) -> Result<String> {
    let options = crate::cli::parse(&args)?;
    let verb = options.words.first().map(String::as_str);
    if verb == Some("system") {
        return Ok("{\"status\":\"ok\"}".to_owned());
    }
    if let Some(config) = crate::workspace::resolve(&options)? {
        let cache = crate::workspace::cache_path(&config, options.cache.as_deref())?;
        if verb == Some("stop") {
            refuse_router_stop(&cache)?;
            return daemon::stop(&cache);
        }
        if let Some(reply) = daemon::proxy(&cache, &args, &context)? {
            return Ok(reply);
        }
        let mut server = options.clone();
        server.words = vec!["workspace-serve".into()];
        server.member = None;
        let mut command = Command::new(std::env::current_exe()?);
        command.args(normalized_args(&server, &server.root));
        let child = spawn_background(&mut command)?;
        return forward_spawned(&cache, &args, &context, &child);
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
    if let Some(reply) = daemon::proxy(&cache, &args, &context)? {
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
    forward_spawned(&cache, &args, &context, &child)
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
) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Some(reply) = daemon::proxy(cache, args, context)? {
            return Ok(reply);
        }
        if daemon::spawn_failed(child, cache) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    bail!("daemon_unavailable: target did not start")
}
