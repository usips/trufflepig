//! Workspace coordination shares inference while existing root daemons own reconciliation.
use super::{WorkspaceConfig, cache_path, local, member_cache, member_root::MemberRoot};
use crate::{
    background_process::spawn_background,
    cli::{Arguments, normalized_args},
    daemon,
    diagnostics::RequestContext,
    semantic::SemanticSession,
};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use std::{
    fs::OpenOptions,
    path::Path,
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
        options.budget = budget;
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
    let options = apply_config(&config, options)?;
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
    if options.no_daemon || verb == "ws" || metadata_only {
        return local(
            &config,
            &cache,
            &options,
            context,
            &mut SemanticSession::new(),
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
    if let Some(reply) = daemon::request(&cache, &args, context)? {
        return wait_if_semantic_prepare(&config, &options, reply);
    }
    std::fs::create_dir_all(&cache)?;
    let mut server = options.clone();
    server.words = vec!["workspace-serve".into()];
    server.member = None;
    let mut command = Command::new(std::env::current_exe()?);
    command.args(normalized_args(&server, &server.root));
    let mut child = spawn_background(&mut command)?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(3) {
        if let Some(reply) = daemon::request(&cache, &args, context)? {
            return wait_if_semantic_prepare(&config, &options, reply);
        }
        if child.try_wait()?.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    anyhow::bail!(
        "workspace_unavailable: no daemon reachable and the coordinator did not start; \
         a sandbox that blocks unix sockets or cache writes must run trufflepig \
         unsandboxed; otherwise start the trufflepig-system service, use --no-daemon, \
         or inspect cache permissions"
    )
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
    let mut session = SemanticSession::new();
    let config_path = config.path.clone();
    let config_id = config.id.clone();
    daemon::serve_coordinator(
        config
            .path
            .parent()
            .context("invalid workspace configuration path")?,
        cache,
        |event| match event {
            daemon::DaemonEvent::Request { args, context } => {
                let options = crate::cli::parse(&args)?;
                let current = super::resolve(&options)?.context(
                    "workspace_unavailable: configuration no longer selects this workspace",
                )?;
                ensure!(
                    current.id == config_id && current.path == config_path,
                    "invalid_workspace: coordinator belongs to a different configuration"
                );
                ensure!(
                    cache_path(&current, options.cache.as_deref())? == cache,
                    "invalid_cache: coordinator cache mismatch"
                );
                let output = local(&current, cache, &options, &context, &mut session);
                output
            }
            daemon::DaemonEvent::Idle => {
                crate::background_process::reap_children();
                Ok(String::new())
            }
            daemon::DaemonEvent::Reconcile => Ok(String::new()),
        },
    )
}
/// Starts an unowned root asynchronously. Existing root ownership is left intact.
pub(super) fn ensure_member(member: &MemberRoot, options: &Arguments) -> Result<()> {
    if options.no_daemon {
        return Ok(());
    }
    let cache = member_cache(member, options.cache.as_deref())?;
    std::fs::create_dir_all(&cache)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join("daemon.lock"))?;
    match lock.try_lock_exclusive() {
        Ok(()) => {
            FileExt::unlock(&lock)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let mut local = super::member_options(options, member)?;
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
    let mut command = Command::new(std::env::current_exe()?);
    command.args(normalized_args(&local, &local.root));

    spawn_background(&mut command)?;
    Ok(())
}
