//! Linux command dispatch; stdout contains one budgeted JSON response.
mod dispatch;
pub mod emission;
mod emitted_evidence;
use crate::{daemon, output::OutputBudget, store::Store};
use anyhow::{Context, Result};
pub use dispatch::local;
use dispatch::local_with_session;
mod arguments;
pub use arguments::{Arguments, parse};
use arguments::{normalized_args, validate};
#[cfg(test)]
mod emission_tests;
#[cfg(test)]
mod tests;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
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
    let root = options
        .root
        .canonicalize()
        .context("invalid_root: cannot open repository root")?;
    let cache = cache_path(&root, options.cache.as_deref())?;
    let verb = options
        .words
        .first()
        .map(String::as_str)
        .unwrap_or("status");
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
                );
                semantic_session.request_completed();
                if let Some(sample) = semantic_session.idle_tick() {
                    let mut event = crate::diagnostics::RequestEvent::new(
                        crate::diagnostics::RequestContext::new(None, None),
                        crate::diagnostics::Operation::Other,
                        crate::diagnostics::Outcome::Success,
                    );
                    event.stage = crate::diagnostics::EventStage::Maintenance;
                    event.residency = Some(sample);
                    if let Some(queue) = &log_queue {
                        queue.record(event);
                    }
                }
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
                Ok(String::new())
            }
            daemon::DaemonEvent::Idle => {
                if let Some(sample) = semantic_session.idle_tick() {
                    let mut event = crate::diagnostics::RequestEvent::new(
                        crate::diagnostics::RequestContext::new(None, None),
                        crate::diagnostics::Operation::Other,
                        crate::diagnostics::Outcome::Success,
                    );
                    event.stage = crate::diagnostics::EventStage::Maintenance;
                    event.residency = Some(sample);
                    if let Some(queue) = &log_queue {
                        queue.record(event);
                    }
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
    if !options.no_daemon && !matches!(verb, "index" | "init" | "semantic-check") {
        if let Some(response) = daemon::request(&cache, &normalized_args(&options, &root), context)?
        {
            return wait_for_history(&root, &options, response);
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
        command.arg("--diagnostics").arg(&options.diagnostics);
        let mut child = command
            .arg("--root")
            .arg(&root)
            .arg("--cache")
            .arg(&cache)
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Some(response) =
                daemon::request(&cache, &normalized_args(&options, &root), context)?
            {
                return wait_for_history(&root, &options, response);
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
    )
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
    let cache = crate::history::worker::resolve_cache(
        root,
        options.cache.as_deref(),
        options.history_cache.as_deref(),
    )?;
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
