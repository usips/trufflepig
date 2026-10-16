//! Local dispatch against published live and captured historical identities.
use super::{Arguments, emission, request_context, validate};
use crate::{
    daemon::deadline::QueryDeadline,
    output::OutputBudget,
    store::{
        IndexProgressPhase, Store, index_cache_unavailable, is_index_cache_unavailable,
        is_index_cannot_open, is_index_warming, preflight_results_cache,
    },
};
use anyhow::{Context, Result, bail, ensure};
use std::path::Path;

#[cfg(test)]
mod cache_read_tests;
#[cfg(test)]
mod cache_recovery_tests;
mod standalone_worktree_reads;

pub fn local(
    root: &Path,
    cache: &Path,
    options: &Arguments,
    daemon_running: bool,
) -> Result<String> {
    let context = request_context(options)?;
    local_with_session(
        root,
        cache,
        options,
        daemon_running,
        &mut crate::semantic::SemanticSession::default(),
        &context,
        None,
        None,
        QueryDeadline::start(),
    )
}

pub(super) fn local_with_session(
    root: &Path,
    cache: &Path,
    options: &Arguments,
    daemon_running: bool,
    session: &mut crate::semantic::SemanticSession,
    context: &crate::diagnostics::RequestContext,
    log_queue: Option<&crate::diagnostics::DiagnosticQueue>,
    preparation_manager: Option<&crate::semantic::preparation::PreparationManager>,
    deadline: QueryDeadline,
) -> Result<String> {
    let initializations = crate::semantic::model_initializations();
    let started = std::time::Instant::now();
    let result = local_dispatch(
        root,
        cache,
        options,
        daemon_running,
        session,
        context,
        log_queue,
        preparation_manager,
        deadline,
    );
    let unexpected = crate::semantic::model_initializations().saturating_sub(initializations);
    if unexpected > 0
        && options.diagnostics != "off"
        && !options.sem
        && options
            .words
            .first()
            .is_none_or(|verb| verb != "semantic-check")
    {
        let mut event = crate::diagnostics::RequestEvent::new(
            context.clone(),
            emission::operation(options),
            crate::diagnostics::Outcome::Failure,
        );
        event.stage = crate::diagnostics::EventStage::Maintenance;
        event.probes.push(crate::probes::ProbeResult {
            name: crate::probes::ProbeName::ModelInitialization,
            outcome: crate::probes::ProbeOutcome::Failed,
            checked: unexpected as usize,
            drifted: 0,
            unverified: 0,
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
        if let Some(queue) = log_queue {
            queue.record(event);
        } else {
            crate::diagnostics::best_effort_record(
                cache,
                emission::diagnostics_mode(options),
                event,
            );
        }
    }
    result
}

fn local_dispatch(
    root: &Path,
    cache: &Path,
    options: &Arguments,
    daemon_running: bool,
    session: &mut crate::semantic::SemanticSession,
    context: &crate::diagnostics::RequestContext,
    log_queue: Option<&crate::diagnostics::DiagnosticQueue>,
    preparation_manager: Option<&crate::semantic::preparation::PreparationManager>,
    deadline: QueryDeadline,
) -> Result<String> {
    validate(options)?;
    session.set_no_daemon(options.no_daemon);
    if options.root.canonicalize()? != root {
        bail!("invalid_root: daemon cache belongs to another repository");
    }
    let budget = OutputBudget::new(options.budget)?;
    let page_budget = OutputBudget::new(options.budget)?.with_format(options.output_format());
    let verb = options
        .words
        .first()
        .map(String::as_str)
        .unwrap_or("status");
    let bounded_read = matches!(
        verb,
        "search" | "refs" | "map" | "show" | "more" | "ctx" | "status" | "doctor"
    );
    if bounded_read && deadline.expired() {
        bail!(
            "{}: query deadline expired",
            crate::daemon::deadline::TIMED_OUT
        );
    }
    if verb == "show"
        && let Some(target) = crate::source::acquisition::show_target(&options.words)
        && crate::source::acquisition::is_current_path_target(&target)
    {
        let side = options
            .side
            .as_deref()
            .map(crate::source::SourceSide::parse)
            .transpose()?;
        if side.is_some() {
            bail!("invalid_side: current path reads have no historical side");
        }
        if deadline.expired() {
            bail!(
                "{}: query deadline expired",
                crate::daemon::deadline::TIMED_OUT
            );
        }
        let store = Store::unpublished(root, cache).map_err(|error| deadline.classify(error))?;
        if deadline.expired() {
            bail!(
                "{}: query deadline expired",
                crate::daemon::deadline::TIMED_OUT
            );
        }
        let origin = crate::search::InvocationDirectory::within(store.root.as_path(), root);
        let acquired = crate::source::acquisition::acquire_current_path_without_cache(
            &store, &target, side, &origin,
        )
        .map_err(|error| deadline.classify(error))?;
        if deadline.expired() {
            bail!(
                "{}: query deadline expired",
                crate::daemon::deadline::TIMED_OUT
            );
        }
        let rendered = crate::source::render_owned(acquired, &page_budget, &serde_json::json!({}))
            .map_err(|error| deadline.classify(error))?;
        if deadline.expired() {
            bail!(
                "{}: query deadline expired",
                crate::daemon::deadline::TIMED_OUT
            );
        }
        return Ok(rendered);
    }
    if verb == "semantic" {
        return super::semantic::local(root, cache, options, preparation_manager);
    }
    if verb == "semantic-check" {
        let path = options
            .words
            .get(1)
            .context("usage: semantic-check MODEL_DIRECTORY")?;
        return match options.words.get(2).map(String::as_str) {
            None => budget.render(&crate::semantic::run_gate(Path::new(path))?),
            Some("cuda") => {
                ensure_semantic_check_arity(options)?;
                let ordinal = options
                    .words
                    .get(3)
                    .context("usage: semantic-check MODEL_DIRECTORY cuda DEVICE_ORDINAL")?
                    .parse::<i32>()
                    .context("invalid CUDA device ordinal")?;
                #[cfg(feature = "semantic")]
                {
                    budget.render(&crate::semantic::run_gpu_gate(
                        Path::new(path),
                        crate::semantic::SemanticProviderConfig::cuda(ordinal),
                    )?)
                }
                #[cfg(not(feature = "semantic"))]
                {
                    let _ = ordinal;
                    Err(crate::semantic::unavailable())
                }
            }
            Some(provider) => bail!(
                "usage: semantic-check MODEL_DIRECTORY [cuda DEVICE_ORDINAL] (got {provider})"
            ),
        };
    }
    let (mut store, mut parent_fallback) =
        if matches!(verb, "show" | "more" | "ctx" | "search" | "refs" | "map") {
            read_store(root, cache, options, daemon_running, &deadline)?
        } else if verb == "status" {
            let store = match Store::open_read(root, cache, deadline.clone())
                .map_err(|error| classify_query_error(&deadline, error))
            {
                Err(error) if is_index_warming(&error) => {
                    if deadline.expired() {
                        bail!(
                            "{}: query deadline expired",
                            crate::daemon::deadline::TIMED_OUT
                        );
                    }
                    return warming_status(root, cache, &budget);
                }
                Err(error) if is_index_cache_unavailable(&error) => {
                    return Err(index_cache_unavailable(cache, error));
                }
                opened => opened?,
            };
            (store, None)
        } else if verb == "doctor" {
            let store = match Store::open_read(root, cache, deadline.clone())
                .map_err(|error| classify_query_error(&deadline, error))
            {
                Ok(store) => store,
                Err(error) if is_index_warming(&error) => {
                    Store::unpublished(root, cache)?.with_query_deadline(deadline.clone())
                }
                Err(error) => return Err(index_cache_unavailable(cache, error)),
            };
            (store, None)
        } else {
            (Store::open(root, cache)?, None)
        };
    let argument = || {
        options
            .words
            .get(1)
            .map(String::as_str)
            .context("usage: command requires an explicit argument")
    };
    if matches!(
        verb,
        "hist-index" | "hist-status" | "hist" | "since" | "diff" | "blame"
    ) {
        let history_cache = options
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
        let mut history = crate::history::History::open(root, &history_cache)?;
        if options.no_daemon && verb == "hist" {
            history.index()?;
        }
        return match verb {
            "hist-status" => budget.render(&history.status()?),
            "hist-index" => {
                if options.no_daemon || options.wait {
                    let mut status = history.index()?;
                    while options.wait && status["complete"] == false && status["failure"].is_null()
                    {
                        status = history.index()?;
                    }
                    budget.render(&status)
                } else {
                    let status = history.schedule()?;
                    crate::history::worker::start(root, &history_cache)?;
                    budget.render(&status)
                }
            }
            "hist" => history.hist(&mut store, argument()?, &budget),
            "since" => history.since(
                &mut store,
                argument()?,
                options.words.get(2).map(String::as_str),
                options.uncommitted,
                &budget,
            ),
            "diff" => history.diff(&mut store, argument()?, options.target.as_deref(), &budget),
            "blame" => history.blame(
                &mut store,
                argument()?,
                options.raw,
                options.ignore_revs_file.as_deref(),
                &budget,
            ),
            _ => unreachable!(),
        };
    }
    if matches!(verb, "session" | "audit" | "forget-logs") {
        let diagnostics =
            crate::diagnostics::DiagnosticStore::open(cache, emission::diagnostics_mode(options))?;
        return match verb {
            "session" => match argument()? {
                "start" => budget.render(&diagnostics.start_session(&mut store)?),
                "end" => {
                    let report = diagnostics.end_session(
                        &mut store,
                        options.words.get(2).context("usage: session end ID")?,
                    )?;
                    budget.render(&report).or_else(|_| budget.render(&serde_json::json!({
                        "session":report["session"],"status":report["status"],
                        "before_generation":report["before_generation"],"after_generation":report["after_generation"],
                        "changed_files":report["changes"].as_array().map_or(0,Vec::len),
                        "changes_truncated":report["changes_truncated"],
                        "overlapping_observation":report["overlapping_observation"],
                        "details":"audit SESSION_ID", "output_truncated":true
                    })))
                }
                _ => bail!("usage: session start | session end ID"),
            },
            "audit" => diagnostics.render_audit(&budget, options.words.get(1).map(String::as_str)),
            "forget-logs" => {
                diagnostics.forget()?;
                budget.render(&serde_json::json!({"status":"forgotten"}))
            }
            _ => unreachable!(),
        };
    }
    let response = match verb {
        "index"|"init"=>{let coverage=store.index()?;budget.render(&serde_json::json!({"generation":store.generation()?,"coverage":coverage}))},
        "doctor"=>budget.render(&crate::probes::doctor(&store, cache, session)?),
        "status"=>budget.render(&serde_json::json!({"generation":store.generation()?,"coverage":store.coverage()?,"semantic_feature":cfg!(feature="semantic"),"tokenizer":"o200k_base"})),
        "show"=>standalone_worktree_reads::show_read(root,cache,store,parent_fallback.take(),options,&page_budget,deadline.clone()),
        "more"=>standalone_worktree_reads::more_read(&store,argument()?,options.limit,&page_budget),
        "ctx"=>standalone_worktree_reads::context_read(root,cache,&store,argument()?,deadline.clone(),&budget),
        "search" | "refs" | "map" => {
            standalone_worktree_reads::retrieve_read(root,cache,&store,parent_fallback.take(),verb,options,session,context,log_queue,preparation_manager,&page_budget,&deadline)
        }
        _ => bail!("invalid_command: unknown command {verb}; use search for queries"),
    };
    let response = response.map_err(|error| deadline.classify(error))?;
    if bounded_read && deadline.expired() {
        bail!(
            "{}: query deadline expired",
            crate::daemon::deadline::TIMED_OUT
        );
    }
    Ok(response)
}

/// Opens a query-only store and reconciles index-backed local reads within the
/// request deadline. Eligible linked-worktree reads try their verified parent first.
fn read_store(
    root: &Path,
    cache: &Path,
    options: &Arguments,
    daemon_running: bool,
    deadline: &QueryDeadline,
) -> Result<(Store, Option<crate::workspace::ParentFallback>)> {
    let verb = options.words.first().map_or("status", String::as_str);
    if matches!(verb, "search" | "refs" | "map" | "show") {
        preflight_results_cache(cache, Some(deadline))
            .map_err(|error| classify_query_error(deadline, error))?;
    }
    let reads_without_index = standalone_worktree_reads::can_read_without_index(options);
    let opened = Store::open_read(root, cache, deadline.clone())
        .map_err(|error| classify_query_error(deadline, error));
    let (opened_store, open_error) = match opened {
        Ok(store) => (Some(store), None),
        Err(error) => {
            if reads_without_index
                && (is_index_warming(&error)
                    || is_index_cache_unavailable(&error)
                    || is_index_cannot_open(&error))
            {
                return Ok((
                    Store::unpublished(root, cache)?.with_query_deadline(deadline.clone()),
                    None,
                ));
            }
            (None, Some(error))
        }
    };

    let mut parent_view = open_error
        .as_ref()
        .and_then(|error| try_standalone_parent(root, cache, options, error, deadline));
    if !options.no_daemon
        && let Some(view) = parent_view.take()
    {
        return Ok((view.store, Some(view.fallback)));
    }

    let query_needs_fresh_local_index = matches!(verb, "search" | "refs" | "map")
        || verb == "show"
            && crate::source::acquisition::show_target(&options.words)
                .is_some_and(|target| target.starts_with("sym:"));
    let open_error_can_reconcile = open_error.as_ref().is_some_and(|error| {
        is_index_warming(error) || is_index_cache_unavailable(error) || is_index_cannot_open(error)
    });
    let needs_reconcile = !reads_without_index
        && ((query_needs_fresh_local_index && opened_store.is_some()) || open_error_can_reconcile);
    if deadline.expired() {
        bail!(
            "{}: query deadline expired",
            crate::daemon::deadline::TIMED_OUT
        );
    }
    let reconciles = if !needs_reconcile || daemon_running {
        false
    } else if options.no_daemon {
        true
    } else {
        !crate::daemon::running_by(cache, deadline)?
    };
    if reconciles {
        let preserve_parent_response = verb == "show"
            && crate::source::acquisition::show_target(&options.words)
                .is_some_and(|target| target.starts_with("sym:"));
        return reconcile_inline(
            root,
            cache,
            deadline,
            opened_store,
            parent_view,
            preserve_parent_response,
        );
    }

    if opened_store.is_none()
        && let Some(view) = parent_view.take()
    {
        return Ok((view.store, Some(view.fallback)));
    }
    match opened_store {
        Some(store) => Ok((store, None)),
        None => Err(index_cache_unavailable(
            cache,
            open_error.expect("a missing store has an open error"),
        )),
    }
}

fn classify_query_error(deadline: &QueryDeadline, error: anyhow::Error) -> anyhow::Error {
    let error = deadline.classify(error);
    if deadline.expired() && !crate::daemon::deadline::is_timed_out(&error) {
        error.context(format!(
            "{}: query deadline expired",
            crate::daemon::deadline::TIMED_OUT
        ))
    } else {
        error
    }
}

fn reconcile_inline(
    root: &Path,
    cache: &Path,
    deadline: &QueryDeadline,
    own_view: Option<Store>,
    mut parent_view: Option<crate::workspace::ParentIndexView>,
    preserve_parent_response: bool,
) -> Result<(Store, Option<crate::workspace::ParentFallback>)> {
    if deadline.expired() {
        bail!(
            "{}: query deadline expired",
            crate::daemon::deadline::TIMED_OUT
        );
    }
    let maintenance = crate::store::inline_maintenance_deadline(deadline);
    if own_view.is_none()
        && let Some(parent) = parent_view.as_ref()
    {
        let _ = crate::store::ensure_seeded_until(
            parent.store.index_cache(),
            root,
            cache,
            &maintenance,
        );
        if deadline.expired() {
            bail!(
                "{}: query deadline expired",
                crate::daemon::deadline::TIMED_OUT
            );
        }
    }

    if maintenance.expired() {
        return available_or_warming(
            root,
            cache,
            own_view,
            parent_view,
            "inline maintenance budget is exhausted",
        );
    }

    let mut writer = match Store::open_for_reconcile(root, cache, maintenance.clone()) {
        Ok(writer) => writer,
        Err(error) if maintenance_slice_ended(&maintenance, deadline, &error) => {
            return available_or_warming(
                root,
                cache,
                own_view,
                parent_view,
                "inline maintenance budget expired before publication",
            );
        }
        Err(error)
            if (is_index_warming(&error) || is_index_cache_unavailable(&error))
                && (own_view.is_some() || parent_view.is_some()) =>
        {
            return available_or_warming(
                root,
                cache,
                own_view,
                parent_view,
                "inline maintenance could not acquire a writable view",
            );
        }
        Err(error) => return Err(index_cache_unavailable(cache, error)),
    };

    let attempt = if own_view.is_some() || parent_view.is_some() {
        writer.index_available_until(maintenance.clone())
    } else {
        writer.index_until(maintenance.clone())
    };
    match attempt {
        Ok(Some(_)) => {
            if preserve_parent_response && let Some(view) = parent_view.take() {
                return Ok((view.store, Some(view.fallback)));
            }
            let store = Store::open_read(root, cache, deadline.clone())
                .map_err(|error| classify_query_error(deadline, error))?;
            Ok((store, None))
        }
        Ok(None) => available_or_warming(
            root,
            cache,
            own_view,
            parent_view,
            "index preparation is still in progress or another writer holds the lease",
        ),
        Err(error) if maintenance_slice_ended(&maintenance, deadline, &error) => {
            available_or_warming(
                root,
                cache,
                own_view,
                parent_view,
                "inline maintenance budget expired before publication",
            )
        }
        Err(error) if is_index_warming(&error) && (own_view.is_some() || parent_view.is_some()) => {
            available_or_warming(
                root,
                cache,
                own_view,
                parent_view,
                "index preparation is still in progress",
            )
        }
        Err(error) => Err(index_cache_unavailable(cache, error)),
    }
}

fn maintenance_slice_ended(
    maintenance: &QueryDeadline,
    original: &QueryDeadline,
    error: &anyhow::Error,
) -> bool {
    crate::daemon::deadline::is_timed_out(error) && !original.expired() && maintenance.expired()
}

fn available_or_warming(
    root: &Path,
    cache: &Path,
    own_view: Option<Store>,
    parent_view: Option<crate::workspace::ParentIndexView>,
    reason: &str,
) -> Result<(Store, Option<crate::workspace::ParentFallback>)> {
    if let Some(store) = own_view {
        return Ok((store, None));
    }
    if let Some(view) = parent_view {
        return Ok((view.store, Some(view.fallback)));
    }
    Err(index_warming_error(root, cache, reason))
}

fn index_warming_error(root: &Path, cache: &Path, reason: &str) -> anyhow::Error {
    match crate::store::read_index_progress(root, cache) {
        Some(progress) => {
            let phase = match progress.phase {
                IndexProgressPhase::Scanning => "scanning",
                IndexProgressPhase::Finalizing => "finalizing",
                IndexProgressPhase::MaintenanceRequired => "maintenance_required",
            };
            anyhow::anyhow!(
                "index_warming: {reason}; progress phase={phase}, completed_files={}, reused_files={}, maintenance_required={}; {}; run `trufflepig index` to finish",
                progress.completed_files,
                progress.reused_files,
                progress.maintenance_required,
                progress.description(),
            )
        }
        None => anyhow::anyhow!(
            "index_warming: {reason}; no retained checkpoint progress; run `trufflepig index` to build the index"
        ),
    }
}

fn try_standalone_parent(
    root: &Path,
    cache: &Path,
    options: &Arguments,
    error: &anyhow::Error,
    deadline: &QueryDeadline,
) -> Option<crate::workspace::ParentIndexView> {
    if deadline.expired() {
        return None;
    }
    if !standalone_worktree_reads::can_use_parent(options, error) {
        return None;
    }
    crate::workspace::standalone_parent_view(root, cache, deadline.clone())
}

/// `status` of an unpublished index without opening a writer or creating cache files.
fn warming_status(root: &Path, cache: &Path, budget: &OutputBudget) -> Result<String> {
    let index_progress = crate::store::read_index_progress(root, cache);
    let mut coverage = serde_json::to_value(crate::store::Coverage::default())?;
    if let Some(progress) = &index_progress {
        coverage["index_progress"] = serde_json::to_value(progress)?;
    }
    budget.render(&serde_json::json!({
        "generation": 0,
        "coverage": coverage,
        "state": "warming",
        "index_progress": index_progress,
        "semantic_feature": cfg!(feature = "semantic"),
        "tokenizer": "o200k_base",
    }))
}

fn ensure_semantic_check_arity(options: &Arguments) -> Result<()> {
    ensure!(
        options.words.len() == 4,
        "usage: semantic-check MODEL_DIRECTORY cuda DEVICE_ORDINAL"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source;
    use std::time::Duration;

    fn words(words: &[&str]) -> Arguments {
        super::super::parse(
            &words
                .iter()
                .map(|word| (*word).to_owned())
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }

    /// A cold reader polls a competing writer only within its maintenance slice.
    #[test]
    fn client_reconcile_bounds_a_cold_wait_behind_a_daemon_scan() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("lib.rs"), "pub fn leased() {}\n").unwrap();
        let lease = std::fs::File::create(cache.path().join("index.lock")).unwrap();
        fs2::FileExt::lock_exclusive(&lease).unwrap();
        let started = std::time::Instant::now();
        let error = read_store(
            root.path(),
            cache.path(),
            &words(&["--no-daemon", "search", "sym:leased"]),
            false,
            &QueryDeadline::start(),
        )
        .err()
        .expect("a cold reader cannot publish while the writer lease is held");
        assert!(is_index_warming(&error), "{error:#}");
        assert!(error.to_string().contains("trufflepig index"), "{error:#}");
        assert!(started.elapsed() < Duration::from_secs(3), "{error:#}");
        drop(lease);
    }

    /// Direct path reads need no publication, with or without a serving daemon.
    #[test]
    fn warming_path_reads_leave_the_index_untouched() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("lib.rs"), "pub fn early() {}\n").unwrap();
        let mut deadline = QueryDeadline::start();
        for (args, daemon_running) in [
            (vec!["show", "lib.rs"], true),
            (vec!["show", "lib.rs"], false),
            (vec!["--no-daemon", "show", "lib.rs"], false),
        ] {
            let (store, _) = read_store(
                root.path(),
                cache.path(),
                &words(&args),
                daemon_running,
                &mut deadline,
            )
            .unwrap();
            let shown = source::show(&store, "lib.rs", &OutputBudget::new(2_000).unwrap()).unwrap();
            assert!(shown.contains("pub fn early()"), "{shown}");
            assert!(!cache.path().join("index.sqlite3").exists());
        }
        let error = read_store(
            root.path(),
            cache.path(),
            &words(&["show", "sym:early"]),
            true,
            &mut deadline,
        )
        .err()
        .unwrap();
        assert!(is_index_warming(&error), "{error:#}");
    }
}
