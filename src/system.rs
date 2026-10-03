//! Per-user system daemon routing CLI requests to owning workspace or root daemons.
//! Each request is proxied on its own worker, bounded by `daemon::PROXY_REPLY_WAIT`.

pub mod sweep;
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

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BoardDatabaseMarker {
    database: PathBuf,
}

pub(crate) fn record_board_database(runtime: &Path, database: &Path) -> Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    ensure!(
        database.is_absolute(),
        "invalid_options: board database path must be absolute"
    );
    fs::create_dir_all(runtime)?;
    let temporary = runtime.join(format!("board-backend-{}.pending", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)?;
        let bytes = serde_json::to_vec(&BoardDatabaseMarker {
            database: database.to_owned(),
        })?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, runtime.join("board-backend.json"))?;
        fs::File::open(runtime)?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(temporary);
    result
}

pub(crate) fn validate_board_database(runtime: &Path, database: &Path) -> Result<()> {
    let bytes = match fs::read(runtime.join("board-backend.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("board_unavailable: read database pin"),
    };
    let marker: BoardDatabaseMarker =
        serde_json::from_slice(&bytes).context("board_unavailable: invalid database pin")?;
    let equal = marker.database == database
        || marker
            .database
            .canonicalize()
            .ok()
            .zip(database.canonicalize().ok())
            .is_some_and(|(router, local)| router == local);
    ensure!(
        marker.database.is_absolute() && equal,
        "board_unavailable: local database {} differs from router database {}; restore the router database configuration",
        database.display(),
        marker.database.display()
    );
    Ok(())
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
        return Ok(serde_json::to_string(&serde_json::json!({
            "status": "ok", "board_api": crate::board::BOARD_API, "board_db": database,
        }))?);
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

#[cfg(test)]
mod deadline_tests {
    use super::*;

    #[test]
    fn losing_router_start_cannot_replace_the_live_database_marker() {
        let scratch = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/home/josh/.cache/codex-tmp"));
        fs::create_dir_all(&scratch).unwrap();
        let directory = tempfile::Builder::new().tempdir_in(scratch).unwrap();
        let runtime = directory.path().join("runtime");
        let spool = directory.path().join("spool");
        let live_database = directory.path().join("live.sqlite3");
        let loser_database = directory.path().join("loser.sqlite3");
        let router = |database: &Path| SystemRouter {
            runtime: Some(runtime.clone()),
            cache_base: None,
            sweeps: Mutex::new(SweepClock::default()),
            board: crate::board::BoardHost::with_config(crate::board::BoardConfig::for_database(
                database,
            )),
        };
        let live = router(&live_database);
        let live_runtime = runtime.clone();
        let live_spool = spool.clone();
        let worker =
            std::thread::spawn(move || daemon::serve_router(&live_runtime, &live_spool, live));
        let deadline = Instant::now() + Duration::from_secs(3);
        let ping = ["system".into(), "status".into()];
        let context = RequestContext::new(None, None);
        loop {
            if daemon::request(&runtime, &ping, &context)
                .unwrap()
                .is_some()
            {
                break;
            }
            assert!(Instant::now() < deadline, "live router failed to start");
            std::thread::sleep(Duration::from_millis(10));
        }
        let rejected = daemon::serve_router(&runtime, &spool, router(&loser_database));
        assert!(rejected.is_err());
        let accepted = validate_board_database(&runtime, &live_database);
        let refused = validate_board_database(&runtime, &loser_database);
        daemon::stop(&runtime).unwrap();
        worker.join().unwrap().unwrap();
        accepted.unwrap();
        assert!(refused.is_err());
        assert!(!loser_database.exists());
    }

    #[test]
    fn router_database_marker_refuses_split_local_fallback() {
        let scratch = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/home/josh/.cache/codex-tmp"));
        fs::create_dir_all(&scratch).unwrap();
        let directory = tempfile::Builder::new().tempdir_in(scratch).unwrap();
        let pinned = directory.path().join("router.sqlite3");
        record_board_database(directory.path(), &pinned).unwrap();
        validate_board_database(directory.path(), &pinned).unwrap();
        let other = directory.path().join("client.sqlite3");
        let error = validate_board_database(directory.path(), &other).unwrap_err();
        assert!(error.to_string().contains("differs from router database"));
        assert!(!other.exists());
    }

    #[test]
    fn board_routes_without_workspace_or_owner_daemon() {
        let scratch = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/board-router-tests");
        fs::create_dir_all(&scratch).unwrap();
        let directory = tempfile::Builder::new()
            .prefix("router-")
            .tempdir_in(scratch)
            .unwrap();
        let cache = directory.path().join("owner-cache");
        let database = directory.path().join("data/board.sqlite3");
        let router = SystemRouter {
            runtime: None,
            cache_base: None,
            sweeps: Mutex::new(SweepClock::default()),
            board: crate::board::BoardHost::with_config(crate::board::BoardConfig::for_database(
                &database,
            )),
        };
        let args: Vec<String> = [
            "--root",
            directory.path().join("missing-root").to_str().unwrap(),
            "--workspace",
            directory
                .path()
                .join("missing-workspace.toml")
                .to_str()
                .unwrap(),
            "--cache",
            cache.to_str().unwrap(),
            "board",
            "show",
        ]
        .map(str::to_owned)
        .into();
        let reply = router
            .request(AcceptedRequest {
                args,
                context: RequestContext::new(None, None),
                deadline: QueryDeadline::start(),
            })
            .unwrap();
        let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["result"]["result"], "plans");
        assert!(database.exists());
        assert!(!cache.exists(), "board routing spawned an owner daemon");
    }

    #[test]
    fn router_does_not_reset_an_expired_accepted_deadline() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap().path().join("cache");
        let router = SystemRouter {
            runtime: None,
            cache_base: None,
            sweeps: Mutex::new(SweepClock::default()),
            board: crate::board::BoardHost::default(),
        };
        let args: Vec<String> = [
            "--no-workspace",
            "--root",
            root.path().to_str().unwrap(),
            "--cache",
            cache.to_str().unwrap(),
            "search",
            "bounded",
        ]
        .map(str::to_owned)
        .into();

        let error = router
            .request(AcceptedRequest {
                context: RequestContext::new(None, None),
                args,
                deadline: QueryDeadline::after(Duration::ZERO),
            })
            .unwrap_err();
        assert!(crate::daemon::deadline::is_timed_out(&error), "{error:#}");
        assert!(!cache.exists(), "expired request spawned an owner daemon");
    }
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
