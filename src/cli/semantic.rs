//! Semantic preparation and inference worker command handling.

use super::Arguments;
use crate::{
    output::OutputBudget,
    semantic::{preparation, worker},
    workspace::{self, config::WorkspaceConfig},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

/// Handles a root-scoped `semantic` command.
pub(crate) fn local(
    root: &Path,
    cache: &Path,
    options: &Arguments,
    manager: Option<&preparation::PreparationManager>,
) -> Result<String> {
    let budget = OutputBudget::new(options.budget)?;
    let command = options.words.get(1).map(String::as_str).context(
        "usage: semantic prepare [--wait] | semantic status | semantic worker status|stop",
    )?;
    match command {
        "prepare" => prepare(root, cache, options, manager, &budget),
        "status" => status(root, cache, options, &budget),
        "worker" => bail!("usage: semantic worker status | semantic worker stop"),
        other => bail!(
            "usage: semantic prepare [--wait] | semantic status | semantic worker status|stop (got {other})"
        ),
    }
}

/// Handles a process-wide worker command before workspace/root discovery.
pub(crate) fn worker_command(options: &Arguments) -> Result<String> {
    ensure!(
        !options.no_daemon,
        "invalid_options: --no-daemon cannot query or stop the semantic worker"
    );
    let command = options
        .words
        .get(2)
        .map(String::as_str)
        .context("usage: semantic worker status | semantic worker stop")?;
    ensure!(
        options.words.len() == 3,
        "usage: semantic worker status | semantic worker stop"
    );
    let value = match command {
        "status" => serde_json::to_value(worker::status(&options.root)?)?,
        "stop" => serde_json::to_value(worker::stop(&options.root)?)?,
        other => bail!("usage: semantic worker status | semantic worker stop (got {other})"),
    };
    OutputBudget::new(options.budget)?.render(&value)
}

/// Serves the hidden worker process without resolving a repository or workspace.
pub(crate) fn serve_worker(options: &Arguments) -> Result<String> {
    worker::serve(options.cache.as_deref())?;
    Ok(String::new())
}

fn prepare(
    root: &Path,
    cache: &Path,
    options: &Arguments,
    manager: Option<&preparation::PreparationManager>,
    budget: &OutputBudget,
) -> Result<String> {
    if options.no_daemon {
        let worker = preparation::ForegroundWorker::open(root)?;
        return budget.render(&serde_json::to_value(preparation::run_foreground(
            root, cache, worker,
        )?)?);
    }

    // Start the shared worker before waking a manager so its first background
    // batch does not race worker socket creation.
    let worker = worker::ensure_started(root).ok();
    let receipt = match manager {
        Some(manager) => manager.retry(root, cache)?,
        None => preparation::retry(root, cache)?,
    };
    // Model admission remains asynchronous; only socket/process creation is
    // requested by this command.
    budget.render(&json!({
        "status": "scheduled",
        "captured_generation": receipt.captured_generation,
        "coalesced": receipt.coalesced,
        "worker": worker,
        "wait": options.wait,
    }))
}

fn status(
    root: &Path,
    cache: &Path,
    _options: &Arguments,
    budget: &OutputBudget,
) -> Result<String> {
    let preparation = preparation::status(root, cache)?;
    budget.render(&serde_json::to_value(preparation)?)
}

/// Captures a prepared generation from a schedule response and waits outside a daemon request.
pub(crate) fn wait_for_schedule(
    root: &Path,
    cache: &Path,
    options: &Arguments,
    response: String,
) -> Result<String> {
    if !options.wait || options.no_daemon {
        return Ok(response);
    }
    let scheduled: Value = serde_json::from_str(&response)?;
    let generation = scheduled["captured_generation"]
        .as_i64()
        .context("semantic prepare response omitted captured_generation")?;
    let waited = wait_until_terminal(root, cache, generation)?;
    OutputBudget::new(options.budget)?.render(&serde_json::to_value(waited)?)
}

/// Waits for each member after a workspace coordinator has returned schedules.
pub(crate) fn wait_for_workspace(
    config: &WorkspaceConfig,
    options: &Arguments,
    response: String,
) -> Result<String> {
    if !options.wait || options.no_daemon {
        return Ok(response);
    }
    let mut value: Value = serde_json::from_str(&response)?;
    let statuses = value
        .get("members")
        .and_then(Value::as_array)
        .context("semantic workspace response omitted members")?;
    let mut waited = Vec::with_capacity(statuses.len());
    for member in statuses {
        let name = member["member"]
            .as_str()
            .context("semantic workspace response omitted member")?;
        let configured = config
            .member_roots(&options.root.canonicalize()?)?
            .into_iter()
            .find(|candidate| candidate.name() == name)
            .with_context(|| format!("invalid_member: member is not configured: {name}"))?;
        let root = configured.root.clone();
        let cache = workspace::member_cache(&configured, options.cache.as_deref())?;
        let generation = member["captured_generation"].as_i64().or_else(|| {
            member
                .get("preparation")
                .and_then(|status| status["generation"].as_i64())
        });
        if let Some(generation) = generation {
            waited.push(json!({
                "member": member["member"].clone(),
                "status": wait_until_terminal(&root, &cache, generation)?,
            }));
        }
    }
    value["waited"] = Value::Array(waited);
    OutputBudget::new(options.budget)?.render(&value)
}

fn wait_until_terminal(
    root: &Path,
    cache: &Path,
    generation: i64,
) -> Result<preparation::PreparationWait> {
    let started = std::time::Instant::now();
    loop {
        let mut waited =
            preparation::wait_timeout(root, cache, generation, Duration::from_secs(1))?;
        if waited.status.error.as_deref() != Some("preparation_wait_timeout") {
            return Ok(waited);
        }
        if !root_daemon_running(cache)? || started.elapsed() >= Duration::from_secs(3600) {
            waited.state = preparation::PreparationState::Failed;
            waited.status.state = preparation::PreparationState::Failed;
            waited.status.error = Some(
                if started.elapsed() >= Duration::from_secs(3600) {
                    "preparation_wait_deadline"
                } else {
                    "preparation_daemon_stopped"
                }
                .into(),
            );
            return Ok(waited);
        }
    }
}

fn root_daemon_running(cache: &Path) -> Result<bool> {
    use fs2::FileExt;
    let file = match std::fs::File::open(cache.join("daemon.lock")) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    match file.try_lock_exclusive() {
        Ok(()) => {
            FileExt::unlock(&file)?;
            Ok(false)
        }
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests;

/// Executes semantic commands for the selected members of a workspace.
pub(crate) fn workspace(
    config: &WorkspaceConfig,
    options: &Arguments,
    context: &crate::diagnostics::RequestContext,
) -> Result<String> {
    let command = options
        .words
        .get(1)
        .map(String::as_str)
        .context("usage: semantic prepare [--wait] | semantic status")?;
    ensure!(
        matches!(command, "prepare" | "status"),
        "usage: semantic prepare [--wait] | semantic status"
    );
    let members = selected_members(config, options)?;
    let mut values = Vec::with_capacity(members.len());
    for member in &members {
        member.verify_identity()?;
        let cache = workspace::member_cache(member, options.cache.as_deref())?;
        let mut local = options.clone();
        local.root = member.root.clone();
        local.cache = Some(cache.clone());
        local.workspace = None;
        local.no_workspace = true;
        local.member = None;
        // The workspace client owns the aggregate wait. Keeping this member
        // request nonblocking prevents a coordinator socket from being held
        // while a root preparation sweep runs.
        if command == "prepare" && !local.no_daemon {
            local.wait = false;
        }
        local.words = vec!["semantic".into(), command.into()];
        // Member roots use their own daemon and cache. A workspace coordinator
        // never embeds source itself.
        let output = crate::cli::run_with_context(
            &crate::cli::normalized_args(&local, &local.root),
            context,
        )?;
        let mut value: Value = serde_json::from_str(&output)?;
        value["member"] = member.name().into();
        values.push(value);
    }
    let rendered = json!({"workspace": config.name, "command": command, "members": values});
    OutputBudget::new(options.budget)?.render(&rendered)
}

fn selected_members(
    config: &WorkspaceConfig,
    options: &Arguments,
) -> Result<Vec<workspace::member_root::MemberRoot>> {
    let roots = config.member_roots(&options.root.canonicalize()?)?;
    if let Some(name) = &options.member {
        let member = roots
            .into_iter()
            .find(|candidate| candidate.name() == name)
            .with_context(|| format!("invalid_member: member is not configured: {name}"))?;
        return Ok(vec![member]);
    }
    Ok(roots)
}
