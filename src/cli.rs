//! Linux command dispatch; stdout contains one budgeted JSON response.
mod board_api_probe;
mod dispatch;
pub mod emission;
mod emitted_evidence;
mod retry;
mod root_daemon;
pub(crate) mod semantic;
use crate::{
    background_process::spawn_background,
    daemon::{
        self, CLIENT_REPLY_WAIT,
        deadline::{QUERY_DEADLINE, QueryDeadline},
    },
    output::OutputBudget,
    store::Store,
};
use anyhow::{Context, Result};
pub use dispatch::local;
use dispatch::local_with_session;
mod arguments;
pub(crate) use arguments::normalized_args;
use arguments::validate;
pub use arguments::{Arguments, MAX_BUDGET, SHOW_BUDGET, parse};
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
    if let Some(answer) = board_api_probe::probe_board_api(args) {
        return answer;
    }
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

/// Runs a client command, through the system router when the verb routes there.
/// A router error is the answer; only an absent router falls through to
/// [`run_direct`]'s coordinator, root-daemon, and local paths.
pub fn run_with_context(
    args: &[String],
    context: &crate::diagnostics::RequestContext,
) -> Result<String> {
    if let Some(answer) = board_api_probe::probe_board_api(args) {
        return answer;
    }
    let options = parse(args)?;
    validate(&options)?;
    let verb = options
        .words
        .first()
        .map(String::as_str)
        .unwrap_or("status");
    if verb == "semantic-worker-serve" {
        return semantic::serve_worker(&options);
    }
    if verb == "semantic" && options.words.get(1).is_some_and(|verb| verb == "worker") {
        return semantic::worker_command(&options);
    }
    if verb == "ws" && options.words.get(1).is_some_and(|v| v == "discover") {
        return crate::workspace::discover_paths(&options.words[2..], options.budget);
    }
    if verb == "system-serve" {
        return crate::system::serve().map(|()| String::new());
    }
    if verb == "system" {
        return system_command(&options, context);
    }
    if matches!(verb, "board" | "feedback") {
        return crate::board::run_client(args, &options, context);
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
        let routed = crate::system::request(&forwarded, context).and_then(|reply| match reply {
            Some(reply) => Ok(Some(reply)),
            None => {
                let _ = crate::system::ensure();
                crate::system::request(&forwarded, context)
            }
        });
        match routed {
            Ok(Some(reply)) => return system_reply(&applied, &root, verb, config.as_ref(), reply),
            // A router that could not reach or start the owner leaves direct
            // execution; any other router error is the answer.
            Err(error) if !format!("{error:#}").contains("daemon_unavailable") => {
                return Err(error);
            }
            _ => {}
        }
    }
    direct(&options, context, CLIENT_REPLY_WAIT)
}

/// Runs `args` without the system router. Daemons issue their sub-requests
/// (workspace owner verbs, member semantic commands) here, so a request never
/// re-enters the router that is still proxying it; `reply_wait` keeps their
/// root-daemon waits inside the wait of whoever forwarded the request.
pub(crate) fn run_direct(
    args: &[String],
    context: &crate::diagnostics::RequestContext,
    reply_wait: Duration,
) -> Result<String> {
    let options = parse(args)?;
    validate(&options)?;
    direct(&options, context, reply_wait)
}

fn direct(
    options: &Arguments,
    context: &crate::diagnostics::RequestContext,
    reply_wait: Duration,
) -> Result<String> {
    let replying = QueryDeadline::after(reply_wait);
    let verb = options
        .words
        .first()
        .map(String::as_str)
        .unwrap_or("status");
    if !matches!(verb, "serve" | "history-serve") {
        if let Some(config) = crate::workspace::resolve(options)? {
            return crate::workspace::run(config, options, context);
        }
        if options.member.is_some() || verb == "ws" {
            anyhow::bail!("workspace_required: select a workspace configuration");
        }
    }
    let root = options
        .root
        .canonicalize()
        .context("invalid_root: cannot open repository root")?;
    let cache = cache_path(&root, options.cache.as_deref())?;
    if verb != "history-serve" {
        seed_linked_worktree_cache(&root, &cache);
    }
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
        return root_daemon::serve(&root, &cache, options).map(|()| String::new());
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
        let args = normalized_args(options, &root);
        if let Some(response) = daemon::request_by(&cache, &args, context, replying)? {
            return root_reply(&root, &cache, options, response);
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
        let child = spawn_background(
            command
                .arg("--root")
                .arg(&root)
                .arg("--cache")
                .arg(&cache)
                .arg("serve"),
        )?;
        let deadline = Instant::now() + replying.cap(Duration::from_secs(3));
        while Instant::now() < deadline {
            if let Some(response) = daemon::request_by(&cache, &args, context, replying)? {
                return root_reply(&root, &cache, options, response);
            }
            if daemon::spawn_failed(&child, &cache) {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        // A daemon that did not start leaves local SQLite access coherent.
    }
    local_with_session(
        &root,
        &cache,
        options,
        false,
        &mut crate::semantic::SemanticSession::default(),
        context,
        None,
        None,
        replying.capped(QUERY_DEADLINE),
    )
}

/// Applies to a root daemon's reply the waits that run outside the daemon.
fn root_reply(root: &Path, cache: &Path, options: &Arguments, response: String) -> Result<String> {
    let response = wait_for_history(root, options, response)?;
    if options.words.first().is_some_and(|verb| verb == "semantic") {
        semantic::wait_for_schedule(root, cache, options, response)
    } else {
        Ok(response)
    }
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
    !(verb == "semantic"
        && options
            .words
            .get(1)
            .is_some_and(|command| command == "status"))
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
        Some("prune") => {
            let evicted = crate::system::sweep::sweep(
                &cache_base()?,
                std::time::SystemTime::now(),
                crate::system::sweep::SWEEP_GRACE,
            );
            render(&serde_json::to_string(
                &serde_json::json!({ "evicted": evicted }),
            )?)
        }
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
            anyhow::bail!(
                "usage: system status | system ensure | system stop | system prune (got {other})"
            )
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

/// Default cache base holding one hashed directory per canonical root.
pub fn cache_base() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .context("cache_unavailable: set --cache or XDG_CACHE_HOME")?;
    Ok(base.join("trufflepig"))
}

pub fn cache_path(root: &Path, explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_owned());
    }
    use std::os::unix::ffi::OsStrExt;
    Ok(cache_base()?.join(blake3::hash(root.as_os_str().as_bytes()).to_hex().as_str()))
}

/// Warms an unindexed linked worktree's cache from its main checkout's default
/// cache before the first `Store::open`; best-effort and never fails the command.
fn seed_linked_worktree_cache(root: &Path, cache: &Path) {
    if cache.join("index.sqlite3").exists() {
        return;
    }
    let Some(main_checkout) = crate::store::linked_worktree_main_checkout(root) else {
        return;
    };
    let Ok(member_cache) = cache_path(&main_checkout, None) else {
        return;
    };
    if member_cache.join("index.sqlite3").is_file() {
        crate::store::ensure_seeded(&member_cache, root, cache);
    }
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
