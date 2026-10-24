//! Workspace coordination shares inference while existing root daemons own reconciliation.
//! The coordinator answers requests concurrently; each builds its own session.
#[cfg(test)]
use super::member_cache;
use super::{
    WorkspaceConfig, cache_path, hashed_member_cache, local, member_root::MemberRoot,
    result_cache::WorkspaceResults,
};
use crate::coded_error::CodedContext;
use crate::{
    background_process::{BackgroundOwner, current_background_executable, spawn_background_for},
    cli::{Arguments, normalized_args},
    daemon::{self, AcceptedRequest, DaemonHandler, deadline::QueryDeadline},
    diagnostics::RequestContext,
    semantic::SemanticSession,
};
use anyhow::{Context, Result, ensure};
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

/// Applies workspace config defaults and absolutizes path options for routing.
pub(crate) fn apply_config(config: &WorkspaceConfig, options: &Arguments) -> Result<Arguments> {
    let mut options = options.clone();
    options.sem = (options.sem || config.semantic.enabled) && !options.no_sem;
    options.rerank = (options.rerank || config.semantic.rerank) && !options.no_rerank;
    if !options.explicit_budget
        && let Some(budget) = config.output.budget
    {
        // A workspace page budget never shrinks the larger default source read.
        options.budget = if options.is_show() {
            budget.max(crate::cli::SHOW_BUDGET)
        } else {
            budget
        };
    }
    options.root = options.root.canonicalize()?;
    options.workspace = Some(config.path.clone());
    if let Some(cache) = &options.cache {
        options.cache = Some(std::path::absolute(cache)?);
    }
    if let Some(history_cache) = &options.history_cache {
        options.history_cache = Some(std::path::absolute(history_cache)?);
    }
    if let Some(ignore) = &options.ignore_revs_file {
        options.ignore_revs_file = Some(std::path::absolute(ignore)?);
    }
    Ok(options)
}

pub fn run(
    config: WorkspaceConfig,
    options: &Arguments,
    context: &RequestContext,
) -> Result<String> {
    run_with_deadline(
        config,
        options,
        context,
        QueryDeadline::after(daemon::CLIENT_REPLY_WAIT),
    )
}

/// Runs a workspace command with the caller's absolute request budget.
pub(crate) fn run_with_deadline(
    config: WorkspaceConfig,
    options: &Arguments,
    context: &RequestContext,
    deadline: QueryDeadline,
) -> Result<String> {
    if options
        .words
        .first()
        .is_none_or(|verb| verb != "workspace-serve")
    {
        ensure!(
            !deadline.expired(),
            coded!(TimedOut, "query deadline expired")
        );
    }
    let options = apply_config(&config, options)?;
    let path_deadline = super::local_execution_deadline(&options, &deadline);
    if let Some(rendered) = super::current_path_show(&config, &options, &path_deadline)? {
        return Ok(rendered);
    }
    if options.no_cache && super::stateless_queries::eligible(&options) {
        return super::stateless_queries::run(&config, &options, &path_deadline, None);
    }
    if options.no_daemon
        && super::stateless_queries::eligible(&options)
        && let Some(refusal) =
            super::stateless_queries::cache_refused(&config, &options, &path_deadline)?
    {
        return super::stateless_queries::run(&config, &options, &path_deadline, Some(refusal));
    }
    let cache = cache_path(&config, options.cache.as_deref())?;
    let verb = options
        .words
        .first()
        .map(String::as_str)
        .unwrap_or("status");
    if verb == "workspace-serve" {
        return serve(&config, &cache).map(|()| String::new());
    }
    if verb == "stop" {
        return crate::output::OutputBudget::new(options.budget)?.render(&serde_json::from_str::<
            serde_json::Value,
        >(
            &daemon::stop(&cache)?,
        )?);
    }
    let metadata_only = verb == "semantic"
        && options
            .words
            .get(1)
            .is_some_and(|command| command == "status");
    // Indexing is long foreground work; it never occupies a coordinator worker.
    if options.no_daemon || matches!(verb, "ws" | "index" | "init") || metadata_only {
        if !(verb == "ws" || metadata_only) {
            if let Err(error) = WorkspaceResults::create(&cache, &deadline) {
                let error = deadline.classify(error);
                if options.no_daemon
                    && super::stateless_queries::eligible(&options)
                    && crate::store::is_index_cache_unavailable(&error)
                {
                    let refusal = super::stateless_queries::cache_failure(&cache, &error);
                    return super::stateless_queries::run(
                        &config,
                        &options,
                        &path_deadline,
                        Some(refusal),
                    );
                }
                return Err(error);
            }
        }
        return local(
            &config,
            &cache,
            &options,
            context,
            &mut SemanticSession::new(),
            deadline,
        );
    }
    let mut args = normalized_args(&options, &options.root);
    if options.wait
        && !(verb == "semantic"
            && options
                .words
                .get(1)
                .is_some_and(|command| command == "prepare"))
    {
        args.push("--wait".into());
    }
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    if let Some(reply) = daemon::request_by(&cache, &args, context, deadline.clone())? {
        return wait_if_semantic_prepare(&config, &options, reply);
    }
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    std::fs::create_dir_all(&cache).map_err(|error| {
        let context = anyhow::Error::new(error).with_coded_context(|| {
            coded!(
                CoordinatorCacheCreateFailed,
                "cannot create workspace coordinator cache {}",
                cache.display()
            )
        });
        crate::store::index_cache_unavailable(&cache, context)
    })?;
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    let mut server = options.clone();
    server.words = vec!["workspace-serve".into()];
    server.member = None;
    let executable = current_background_executable(&cache, BackgroundOwner::Command)?;
    let mut command = Command::new(&executable);
    command.args(normalized_args(&server, &server.root));
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    let child = spawn_background_for(&mut command, &executable, "workspace coordinator", &cache)?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(3) && !deadline.expired() {
        if let Some(reply) = daemon::request_by(&cache, &args, context, deadline.clone())? {
            return wait_if_semantic_prepare(&config, &options, reply);
        }
        if child.exited() && !daemon::running_by(&cache, &deadline)? {
            break;
        }
        std::thread::sleep(Duration::from_millis(25).min(deadline.remaining()));
    }
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    anyhow::bail!(coded!(
        WorkspaceUnavailable,
        "coordinator for cache {} did not start; \
         a sandbox that blocks unix sockets or cache writes must run trufflepig \
         unsandboxed; otherwise start the trufflepig-system service, use --no-daemon, \
         or inspect cache permissions",
        cache.display()
    ))
}

fn wait_if_semantic_prepare(
    config: &WorkspaceConfig,
    options: &Arguments,
    reply: String,
) -> Result<String> {
    if options.wait
        && options.words.first().is_some_and(|verb| verb == "semantic")
        && options
            .words
            .get(1)
            .is_some_and(|command| command == "prepare")
    {
        crate::cli::semantic::wait_for_workspace(config, options, reply)
    } else {
        Ok(reply)
    }
}
fn serve(config: &WorkspaceConfig, cache: &Path) -> Result<()> {
    WorkspaceResults::create(cache, &QueryDeadline::after(daemon::CLIENT_REPLY_WAIT))
        .with_coded_context(|| {
            coded!(
                CoordinatorCacheCreateFailed,
                "cannot create workspace coordinator cache {}",
                cache.display()
            )
        })?;
    daemon::serve_coordinator(
        config
            .path
            .parent()
            .context("invalid workspace configuration path")?,
        cache,
        WorkspaceCoordinator {
            config_path: config.path.clone(),
            config_id: config.id.clone(),
            cache: cache.to_owned(),
        },
    )
}

/// Serves one workspace configuration from its coordinator cache.
pub(super) struct WorkspaceCoordinator {
    pub(super) config_path: PathBuf,
    pub(super) config_id: String,
    pub(super) cache: PathBuf,
}

impl DaemonHandler for WorkspaceCoordinator {
    fn request(&self, request: AcceptedRequest) -> Result<String> {
        let options = crate::cli::parse(&request.args)?;
        let current = super::resolve(&options)?.with_coded_context(|| {
            coded!(
                WorkspaceUnavailable,
                "configuration no longer selects this workspace"
            )
        })?;
        ensure!(
            current.id == self.config_id && current.path == self.config_path,
            coded!(
                InvalidWorkspace,
                "coordinator belongs to a different configuration"
            )
        );
        ensure!(
            cache_path(&current, options.cache.as_deref())? == self.cache,
            coded!(InvalidCache, "coordinator cache mismatch")
        );
        local(
            &current,
            &self.cache,
            &options,
            &request.context,
            &mut SemanticSession::new(),
            request.deadline,
        )
    }
}
/// Starts an unowned root asynchronously. Existing root ownership is left intact.
pub(super) fn ensure_member(
    member: &MemberRoot,
    options: &Arguments,
    deadline: &QueryDeadline,
) -> Result<()> {
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    if options.no_daemon {
        return Ok(());
    }
    let cache = hashed_member_cache(&member.root, options.cache.as_deref())?;
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    // Probing the startup lock instead could make a starting daemon exit.
    if daemon::running_by(&cache, deadline)? {
        return Ok(());
    }
    let mut local = super::member_options(options, member, deadline)?;
    std::fs::create_dir_all(&cache).map_err(|error| {
        let context = anyhow::Error::new(error).with_coded_context(|| {
            coded!(
                DaemonCacheCreateFailed,
                "cannot create workspace member cache {}",
                cache.display()
            )
        });
        crate::store::index_cache_unavailable(&cache, context)
    })?;
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    // Resolve from the original override: default worktree history stays shared.
    local.resolved_history_cache = crate::history::worker::resolve_cache(
        &member.root,
        options.cache.as_deref().map(|_| cache.as_path()),
        options.history_cache.as_deref(),
    )
    .ok();
    local.words = vec!["serve".into()];
    local.sem = false;
    local.rerank = false;
    let executable = current_background_executable(&cache, BackgroundOwner::WorkspaceCoordinator)?;
    let mut command = Command::new(&executable);
    command.args(normalized_args(&local, &local.root));

    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    spawn_background_for(&mut command, &executable, "workspace member daemon", &cache)?;
    Ok(())
}

#[cfg(test)]
mod tests;
