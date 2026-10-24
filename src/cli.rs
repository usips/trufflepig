//! Linux command dispatch; stdout contains one budgeted JSON response.
mod board_api_probe;
mod client_command;
use crate::coded_error::CodedContext;
use client_command::ClientCommand;
mod client_identity;
pub use client_identity::request_context;
mod dispatch;
pub mod emission;
mod emitted_evidence;
mod retry;
mod root_daemon;
mod stateless_dispatch;
pub(crate) use root_daemon::{ClassifiedDaemonRequest, classify_daemon_request};
mod system_client_routing;
use system_client_routing::{system_command, system_reply, system_routes};
pub(crate) mod semantic;
use crate::{
    background_process::spawn_background,
    daemon::{self, CLIENT_REPLY_WAIT},
    output::OutputBudget,
    query_deadline::{QUERY_DEADLINE, QueryDeadline},
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
    let context = request_context(&options)?;
    run_with_context(args, &context)
}

/// Runs a client command through its applicable router under a shared read deadline.
/// Absent routers, unavailable owners and local spool timeouts reach direct execution.
/// A pre-dispatch index-cache refusal may route a current-path show to local source.
/// Other router replies remain answers.
pub fn run_with_context(
    args: &[String],
    context: &crate::diagnostics::RequestContext,
) -> Result<String> {
    if let Some(answer) = board_api_probe::probe_board_api(args) {
        return answer;
    }
    let deadline = QueryDeadline::after(CLIENT_REPLY_WAIT);
    ClientCommand::default().run(args, context, deadline)
}

/// Runs `args` without the system router. Daemons issue their sub-requests
/// (workspace owner verbs, member semantic commands) here, so a request never
/// re-enters the router that is still proxying it. The caller's absolute
/// deadline, including peer cancellation, bounds those waits and sub-requests.
pub(crate) fn run_direct(
    args: &[String],
    context: &crate::diagnostics::RequestContext,
    deadline: QueryDeadline,
) -> Result<String> {
    let options = parse(args)?;
    validate(&options)?;
    direct_with_deadline(&options, context, deadline)
}

fn direct_with_deadline(
    options: &Arguments,
    context: &crate::diagnostics::RequestContext,
    deadline: QueryDeadline,
) -> Result<String> {
    let verb = options
        .words
        .first()
        .map(String::as_str)
        .unwrap_or("status");
    let current_path_show = verb == "show"
        && options
            .words
            .get(1)
            .is_some_and(|target| crate::source::acquisition::is_current_path_target(target));
    if !matches!(verb, "serve" | "history-serve") {
        if let Some(config) = crate::workspace::resolve(options)? {
            return crate::workspace::run_with_deadline(config, options, context, deadline.clone());
        }
        if options.member.is_some() || verb == "ws" {
            anyhow::bail!(coded!(
                WorkspaceRequired,
                "select a workspace configuration"
            ));
        }
    }
    let root = options
        .root
        .canonicalize()
        .with_coded_context(|| coded!(InvalidRoot, "cannot open repository root"))?;
    let cache = cache_path(&root, options.cache.as_deref())?;
    if options.no_cache && stateless_dispatch::eligible(options) {
        return stateless_dispatch::run(&root, &cache, options, &deadline.capped(QUERY_DEADLINE));
    }
    let bounded_read = matches!(
        verb,
        "search" | "refs" | "map" | "show" | "more" | "ctx" | "status" | "doctor"
    );
    if verb != "history-serve" && !bounded_read {
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
    if !options.no_daemon
        && !options.no_cache
        && !metadata_only
        && !matches!(verb, "index" | "init" | "semantic-check")
    {
        let args = normalized_args(options, &root);
        if let Some(response) = daemon::request_by(&cache, &args, context, deadline.clone())? {
            return root_reply(&root, &cache, options, response);
        }
        if !deadline.expired() {
            let spawn: Result<crate::background_process::BackgroundChild> = (|| {
                if deadline.expired() {
                    anyhow::bail!(coded!(TimedOut, "query deadline expired"));
                }
                std::fs::create_dir_all(&cache).with_context(|| {
                    format!("cannot create root daemon cache {}", cache.display())
                })?;
                let history_cache = options.resolved_history_cache.clone().or_else(|| {
                    crate::history::worker::resolve_cache(
                        &root,
                        options.cache.as_deref(),
                        options.history_cache.as_deref(),
                    )
                    .ok()
                });
                let executable = crate::background_process::current_background_executable(
                    &cache,
                    crate::background_process::BackgroundOwner::Command,
                )?;
                let mut command = Command::new(&executable);
                if let Some(history_cache) = history_cache {
                    command.arg("--resolved-history-cache").arg(history_cache);
                }
                command
                    .arg("--no-workspace")
                    .arg("--diagnostics")
                    .arg(&options.diagnostics);
                if deadline.expired() {
                    anyhow::bail!(coded!(TimedOut, "query deadline expired"));
                }
                spawn_background(
                    command
                        .arg("--root")
                        .arg(&root)
                        .arg("--cache")
                        .arg(&cache)
                        .arg("serve"),
                )
                .with_context(|| {
                    format!(
                        "cannot spawn root daemon executable {} for cache {}",
                        executable.display(),
                        cache.display()
                    )
                })
            })();
            match spawn {
                Ok(child) => {
                    let spawn_deadline = Instant::now() + deadline.cap(Duration::from_secs(3));
                    while Instant::now() < spawn_deadline {
                        if let Some(response) =
                            daemon::request_by(&cache, &args, context, deadline.clone())?
                        {
                            return root_reply(&root, &cache, options, response);
                        }
                        if child.exited() && !daemon::running_by(&cache, &deadline)? {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(25));
                    }
                }
                Err(_error) if current_path_show => {}
                Err(error) => return Err(error),
            }
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
        deadline.capped(QUERY_DEADLINE),
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

/// Default cache base holding one hashed directory per canonical root.
pub fn cache_base() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .with_coded_context(|| coded!(CacheUnavailable, "set --cache or XDG_CACHE_HOME"))?;
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
    let Some(main_checkout) = crate::store::linked_worktree_main_checkout(
        root,
        crate::workspace::member_root::GIT_PROBE_TIMEOUT,
    ) else {
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
