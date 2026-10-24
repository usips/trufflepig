//! Request routing from the system daemon to workspace, root, and board owners.

use super::{dir, record_board_database};
use crate::coded_error::CodedContext;
use crate::{
    background_process::{
        BackgroundChild, BackgroundOwner, background_executable_replaced,
        current_background_executable, spawn_background_for,
    },
    cli::normalized_args,
    daemon,
    diagnostics::RequestContext,
    query_deadline::QueryDeadline,
};
use anyhow::{Result, bail, ensure};
use std::{
    fs,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

pub(super) fn route(
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
        let mut status = serde_json::json!({
            "status": "ok",
            "board_api": crate::board::BOARD_API,
            "schema_supported": crate::board::SCHEMA_VERSION,
            "process_id": std::process::id(),
        });
        match std::env::current_exe() {
            Ok(executable) => {
                status["executable"] = serde_json::json!(executable.display().to_string());
                status["executable_replaced"] =
                    serde_json::json!(background_executable_replaced(&executable));
            }
            Err(error) => {
                status["executable"] = serde_json::Value::Null;
                status["executable_replaced"] = serde_json::Value::Null;
                status["executable_error"] = serde_json::json!(error.to_string());
            }
        }
        // Board fields are best-effort: status must outlive a broken board
        // configuration, an unwritable pin, or an absent database.
        match board.database_path() {
            Ok(database) if database.is_absolute() => {
                if let Some(runtime) = runtime {
                    let _ = record_board_database(runtime, &database);
                }
                status["board_db"] = serde_json::json!(database);
                if let Some(version) = board_schema_version(&database) {
                    status["schema_file"] = version.into();
                }
            }
            _ => {
                if let Some(error) = board.config_error() {
                    status["board_error"] = serde_json::json!(error);
                }
            }
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
        if let Some(reply) = daemon::request_by(&cache, &args, &context, forwarding.clone())? {
            return Ok(reply);
        }
        let mut server = options.clone();
        server.words = vec!["workspace-serve".into()];
        server.member = None;
        fs::create_dir_all(&cache).map_err(|error| {
            let context = anyhow::Error::new(error).with_coded_context(|| {
                coded!(
                    DaemonCacheCreateFailed,
                    "cannot create workspace daemon cache {}",
                    cache.display()
                )
            });
            crate::store::index_cache_unavailable(&cache, context)
        })?;
        let executable = current_background_executable(&cache, BackgroundOwner::SystemRouter)?;
        let mut command = Command::new(&executable);
        command.args(normalized_args(&server, &server.root));
        ensure!(
            !forwarding.expired(),
            coded!(TimedOut, "query deadline expired")
        );
        let child = spawn_background_for(&mut command, &executable, "workspace daemon", &cache)?;
        return forward_spawned(
            &cache,
            &args,
            &context,
            &child,
            forwarding,
            "workspace daemon",
        );
    }
    let root = options
        .root
        .canonicalize()
        .with_coded_context(|| coded!(InvalidRoot, "cannot open repository root"))?;
    let cache = crate::cli::cache_path(&root, options.cache.as_deref())?;
    if verb == Some("stop") {
        refuse_router_stop(&cache)?;
        return daemon::stop(&cache);
    }
    if let Some(reply) = daemon::request_by(&cache, &args, &context, forwarding.clone())? {
        return Ok(reply);
    }
    fs::create_dir_all(&cache).map_err(|error| {
        let context = anyhow::Error::new(error).with_coded_context(|| {
            coded!(
                DaemonCacheCreateFailed,
                "cannot create root daemon cache {}",
                cache.display()
            )
        });
        crate::store::index_cache_unavailable(&cache, context)
    })?;
    let history_cache = options.resolved_history_cache.clone().or_else(|| {
        crate::history::worker::resolve_cache(
            &root,
            options.cache.as_deref(),
            options.history_cache.as_deref(),
        )
        .ok()
    });
    let executable = current_background_executable(&cache, BackgroundOwner::SystemRouter)?;
    let mut command = Command::new(&executable);
    if let Some(history_cache) = history_cache {
        command.arg("--resolved-history-cache").arg(history_cache);
    }
    command
        .arg("--no-workspace")
        .arg("--diagnostics")
        .arg(&options.diagnostics);
    command
        .arg("--root")
        .arg(&root)
        .arg("--cache")
        .arg(&cache)
        .arg("serve");
    ensure!(
        !forwarding.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    let child = spawn_background_for(&mut command, &executable, "root daemon", &cache)?;
    forward_spawned(&cache, &args, &context, &child, forwarding, "root daemon")
}

/// Best-effort read of the board database's storage schema; absent or
/// unreadable storage reports no version instead of breaking status.
fn board_schema_version(database: &Path) -> Option<i64> {
    let connection =
        rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .ok()
}

/// Refuses a stop aimed at this router's own socket directory.
fn refuse_router_stop(cache: &Path) -> Result<()> {
    if let Some(dir) = dir() {
        ensure!(
            cache != dir,
            coded!(InvalidCache, "cannot stop the system router")
        );
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
    role: &str,
) -> Result<String> {
    ensure!(
        !forwarding.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    let deadline = Instant::now() + forwarding.cap(Duration::from_secs(3));
    while Instant::now() < deadline {
        if let Some(reply) = daemon::request_by(cache, args, context, forwarding.clone())? {
            return Ok(reply);
        }
        if child.exited() && !daemon::running_by(cache, &forwarding)? {
            break;
        }
        std::thread::sleep(Duration::from_millis(25).min(forwarding.remaining()));
    }
    ensure!(
        !forwarding.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    bail!(coded!(
        DaemonUnavailable,
        "{role} for cache {} did not start",
        cache.display()
    ))
}
