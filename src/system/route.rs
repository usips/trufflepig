//! Request routing from the system daemon to workspace, root, and board owners.

use super::{dir, record_board_database};
use crate::{
    background_process::{BackgroundChild, spawn_background},
    cli::normalized_args,
    daemon::{self, deadline::QueryDeadline},
    diagnostics::RequestContext,
};
use anyhow::{Context, Result, bail, ensure};
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
        });
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
