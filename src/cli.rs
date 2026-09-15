//! Linux command dispatch; stdout contains one budgeted JSON response.
mod dispatch;
pub mod emission;
mod emitted_evidence;
pub(crate) mod semantic;
use crate::{background_process::spawn_background, daemon, output::OutputBudget, store::Store};
use anyhow::{Context, Result};
pub use dispatch::local;
use dispatch::local_with_session;
use rusqlite::OptionalExtension;
mod arguments;
pub(crate) use arguments::normalized_args;
use arguments::validate;
pub use arguments::{Arguments, MAX_BUDGET, parse};
#[cfg(test)]
mod emission_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod workspace_emission_tests;
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

pub fn run(args: &[String]) -> Result<String> {
    let options = parse(args)?;
    let context = request_context(&options);
    run_with_context(args, &context)
}

pub fn request_context(options: &Arguments) -> crate::diagnostics::RequestContext {
    crate::diagnostics::RequestContext::new(
        options
            .session
            .clone()
            .or_else(|| std::env::var("TRUFFLEPIG_SESSION").ok()),
        options.client.clone(),
    )
}

pub fn run_with_context(
    args: &[String],
    context: &crate::diagnostics::RequestContext,
) -> Result<String> {
    let options = parse(args)?;
    validate(&options)?;
    let verb = options
        .words
        .first()
        .map(String::as_str)
        .unwrap_or("status");
    if options
        .words
        .first()
        .is_some_and(|verb| verb == "semantic-worker-serve")
    {
        return semantic::serve_worker(&options);
    }
    if options.words.first().is_some_and(|verb| verb == "semantic")
        && options.words.get(1).is_some_and(|verb| verb == "worker")
    {
        return semantic::worker_command(&options);
    }
    if options.words.first().is_some_and(|v| v == "ws")
        && options.words.get(1).is_some_and(|v| v == "discover")
    {
        return crate::workspace::discover_paths(&options.words[2..], options.budget);
    }
    if verb == "system-serve" {
        return crate::system::serve().map(|()| String::new());
    }
    if verb == "system" {
        return system_command(&options, context);
    }
    if system_routes(&options, verb)
        && let Ok(root) = options.root.canonicalize()
        && let Ok(config) = crate::workspace::resolve(&options)
    {
        let applied = match config.as_ref() {
            Some(config) => crate::workspace::apply_config(config, &options)?,
            None => options.clone(),
        };
        let mut forwarded = normalized_args(&applied, &root);
        if config.is_some()
            && options.wait
            && !(verb == "semantic" && options.words.get(1).is_some_and(|c| c == "prepare"))
        {
            forwarded.push("--wait".into());
        }
        match crate::system::request(&forwarded, context) {
            Ok(Some(reply)) => {
                return system_reply(&applied, &root, verb, config.as_ref(), reply);
            }
            Ok(None) => {
                let _ = crate::system::ensure();
                if let Ok(Some(reply)) = crate::system::request(&forwarded, context) {
                    return system_reply(&applied, &root, verb, config.as_ref(), reply);
                }
            }
            Err(_) => {}
        }
    }
    if !options
        .words
        .first()
        .is_some_and(|v| matches!(v.as_str(), "serve" | "history-serve"))
    {
        if let Some(config) = crate::workspace::resolve(&options)? {
            return crate::workspace::run(config, &options, context);
        }
        if options.member.is_some() || options.words.first().is_some_and(|v| v == "ws") {
            anyhow::bail!("workspace_required: select a workspace configuration");
        }
    }
    let root = options
        .root
        .canonicalize()
        .context("invalid_root: cannot open repository root")?;
    let cache = cache_path(&root, options.cache.as_deref())?;
    if verb == "history-serve" {
        return crate::history::worker::serve(
            options
                .history_cache
                .as_deref()
                .context("history worker requires --history-cache")?,
        )
        .map(|()| String::new());
    }
    if verb == "serve" {
        let mut semantic_session = crate::semantic::SemanticSession::default();
        let preparation_manager = crate::semantic::preparation::PreparationManager::new(
            crate::semantic::preparation::SharedWorker::new(&root),
        );
        let history_cache = options.resolved_history_cache.clone().or_else(|| {
            crate::history::worker::resolve_cache(
                &root,
                options.cache.as_deref(),
                options.history_cache.as_deref(),
            )
            .ok()
        });
        let _heartbeat = history_cache
            .as_ref()
            .and_then(|path| crate::history::worker::Heartbeat::start(&root, path).ok());
        let mut log_queue =
            crate::diagnostics::DiagnosticQueue::open(&cache, emission::diagnostics_mode(&options))
                .ok();
        let mut last_probe = Instant::now();
        let mut last_preparation_check = Instant::now();
        daemon::serve(&root, &cache, |request| match request {
            daemon::DaemonEvent::Request { args, context } => {
                let request_options = parse(&args)?;
                let output = local_with_session(
                    &root,
                    &cache,
                    &request_options,
                    true,
                    &mut semantic_session,
                    &context,
                    log_queue.as_ref(),
                    Some(&preparation_manager),
                );
                if output.is_ok()
                    && request_options
                        .words
                        .first()
                        .is_some_and(|verb| verb == "forget-logs")
                {
                    log_queue = crate::diagnostics::DiagnosticQueue::open(
                        &cache,
                        emission::diagnostics_mode(&options),
                    )
                    .ok();
                }
                output
            }
            daemon::DaemonEvent::Reconcile => {
                let mut store = Store::open(&root, &cache)?;
                store.index()?;
                schedule_pending_preparation(&preparation_manager, &root, &cache)?;
                Ok(String::new())
            }
            daemon::DaemonEvent::Idle => {
                if last_preparation_check.elapsed() >= Duration::from_secs(1) {
                    if let Err(error) =
                        schedule_pending_preparation(&preparation_manager, &root, &cache)
                    {
                        eprintln!("trufflepig: semantic preparation scheduling failed: {error}");
                    }
                    last_preparation_check = Instant::now();
                }
                if last_probe.elapsed() >= Duration::from_secs(30) {
                    if let Ok(store) = Store::open(&root, &cache) {
                        if let Ok(report) = crate::probes::doctor(&store, &cache, &semantic_session)
                        {
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
                            if let Some(queue) = &log_queue {
                                queue.record(event);
                            }
                        }
                    }
                    last_probe = Instant::now();
                }
                Ok(String::new())
            }
        })?;
        return Ok(String::new());
    }
    if verb == "stop" {
        Store::open(&root, &cache)?;
        return OutputBudget::new(options.budget)?.render(&serde_json::from_str::<
            serde_json::Value,
        >(&daemon::stop(&cache)?)?);
    }
    let metadata_only = verb == "semantic"
        && options
            .words
            .get(1)
            .is_some_and(|command| command == "status");
    if !options.no_daemon && !metadata_only && !matches!(verb, "index" | "init" | "semantic-check")
    {
        if let Some(response) = daemon::request(&cache, &normalized_args(&options, &root), context)?
        {
            let response = wait_for_history(&root, &options, response)?;
            return if options.words.first().is_some_and(|verb| verb == "semantic") {
                semantic::wait_for_schedule(&root, &cache, &options, response)
            } else {
                Ok(response)
            };
        }
        std::fs::create_dir_all(&cache)?;
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
        let mut child = spawn_background(
            &mut command
                .arg("--root")
                .arg(&root)
                .arg("--cache")
                .arg(&cache)
                .arg("serve"),
        )?;
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Some(response) =
                daemon::request(&cache, &normalized_args(&options, &root), context)?
            {
                let response = wait_for_history(&root, &options, response)?;
                return if options.words.first().is_some_and(|verb| verb == "semantic") {
                    semantic::wait_for_schedule(&root, &cache, &options, response)
                } else {
                    Ok(response)
                };
            }
            if child.try_wait()?.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        // A large startup scan may still own the server; local SQLite access remains coherent.
    }
    local_with_session(
        &root,
        &cache,
        &options,
        false,
        &mut crate::semantic::SemanticSession::default(),
        context,
        None,
        None,
    )
}

/// Routes a verb through the system daemon unless it must run locally.
fn system_routes(options: &Arguments, verb: &str) -> bool {
    if options.no_daemon
        || matches!(
            verb,
            "serve"
                | "history-serve"
                | "workspace-serve"
                | "system-serve"
                | "system"
                | "ws"
                | "stop"
                | "index"
                | "init"
                | "semantic-check"
        )
    {
        return false;
    }
    !(verb == "semantic" && options.words.get(1).is_some_and(|command| command == "status"))
}

/// Handles the `system` verb against the per-user routing daemon.
fn system_command(
    options: &Arguments,
    context: &crate::diagnostics::RequestContext,
) -> Result<String> {
    let render = |json: &str| -> Result<String> {
        OutputBudget::new(options.budget)?.render(&serde_json::from_str::<serde_json::Value>(json)?)
    };
    match options.words.get(1).map(String::as_str) {
        Some("ensure") => {
            crate::system::ensure()?;
            render("{\"status\":\"ok\"}")
        }
        Some("stop") => render(&crate::system::stop()?),
        None | Some("status") => {
            let ping = vec!["system".to_owned(), "status".to_owned()];
            let status = if crate::system::request(&ping, context)
                .ok()
                .flatten()
                .is_some()
            {
                "ok"
            } else {
                "not_running"
            };
            render(&format!("{{\"status\":\"{status}\"}}"))
        }
        Some(other) => {
            anyhow::bail!("usage: system status | system ensure | system stop (got {other})")
        }
    }
}

/// Applies to a system-routed reply the waits a local route would have run.
fn system_reply(
    options: &Arguments,
    root: &Path,
    verb: &str,
    config: Option<&crate::workspace::config::WorkspaceConfig>,
    reply: String,
) -> Result<String> {
    if let Some(config) = config {
        if options.wait
            && verb == "semantic"
            && options
                .words
                .get(1)
                .is_some_and(|command| command == "prepare")
        {
            return semantic::wait_for_workspace(config, options, reply);
        }
        return Ok(reply);
    }
    let reply = wait_for_history(root, options, reply)?;
    if verb == "semantic" {
        let cache = cache_path(root, options.cache.as_deref())?;
        semantic::wait_for_schedule(root, &cache, options, reply)
    } else {
        Ok(reply)
    }
}

/// Re-wakes a manager after indexing when a persisted preparation request was
/// made while no root daemon was serving. The marker keeps preparation opt-in.
fn schedule_pending_preparation(
    manager: &crate::semantic::preparation::PreparationManager,
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

pub fn cache_path(root: &Path, explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_owned());
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .context("cache_unavailable: set --cache or XDG_CACHE_HOME")?;
    use std::os::unix::ffi::OsStrExt;
    Ok(base
        .join("trufflepig")
        .join(blake3::hash(root.as_os_str().as_bytes()).to_hex().as_str()))
}

fn wait_for_history(root: &Path, options: &Arguments, response: String) -> Result<String> {
    if !options.wait
        || options
            .words
            .first()
            .is_none_or(|verb| verb != "hist-index")
    {
        return Ok(response);
    }
    let initial: serde_json::Value = serde_json::from_str(&response)?;
    let cache = options
        .resolved_history_cache
        .clone()
        .map(Ok)
        .unwrap_or_else(|| {
            crate::history::worker::resolve_cache(
                root,
                options.cache.as_deref(),
                options.history_cache.as_deref(),
            )
        })?;
    let mut history = crate::history::History::open(root, &cache)?;
    if let Some(tip) = initial["tip"].as_str() {
        history.tip = crate::identity::GitOid::parse(tip)?;
    }
    let mut status = history.status()?;
    while status["complete"] == false && status["failure"].is_null() {
        std::thread::sleep(Duration::from_millis(100));
        status = history.status()?;
    }
    OutputBudget::new(options.budget)?.render(&status)
}
