//! Local dispatch against published live and captured historical identities.
use super::{Arguments, emission, request_context, validate};
use crate::{output::OutputBudget, results, search, source, store::Store};
use anyhow::{Context, Result, bail, ensure};
use std::path::Path;

pub fn local(
    root: &Path,
    cache: &Path,
    options: &Arguments,
    daemon_running: bool,
) -> Result<String> {
    local_with_session(
        root,
        cache,
        options,
        daemon_running,
        &mut crate::semantic::SemanticSession::default(),
        &request_context(options),
        None,
        None,
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
    let mut store = Store::open(root, cache)?;
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
    match verb {
        "index"|"init"=>{let coverage=store.index()?;budget.render(&serde_json::json!({"generation":store.generation()?,"coverage":coverage}))},
        "doctor"=>budget.render(&crate::probes::doctor(&store, cache, session)?),
        "status"=>budget.render(&serde_json::json!({"generation":store.generation()?,"coverage":store.coverage()?,"semantic_feature":cfg!(feature="semantic"),"tokenizer":"o200k_base"})),
        "show"=>source::show_with_side(&store,argument()?,options.side.as_deref().map(source::SourceSide::parse).transpose()?,&page_budget),
        "more"=>results::more(&store,argument()?,options.limit,&page_budget),
        "ctx"=>search::context(&store,argument()?,&budget),
        "search" | "refs" | "map" => {
            if !daemon_running || store.generation()?==0 {store.index()?;}
            let set=match verb {
                "refs"=>search::references(&store,argument()?)?,
                "map"=>search::map(&store,options.words.get(1).map(String::as_str).unwrap_or(""))?,
                "search"=>{
                    let text=options.words[1..].join(" ");
                    if let Some(name)=text.strip_prefix("refs:"){search::references(&store,name)?}
                    else{{
                        let mut trace = if options.diagnostics == "off" { search::telemetry::RetrievalTrace::disabled() } else { search::telemetry::RetrievalTrace::default() };
                        let query = search::Query::parse(&text)?;
                        let preparation_error = if options.sem && !options.no_daemon && !query.exact && !query.regex {
                            preparation_manager.and_then(|manager| manager.schedule(root, cache).err())
                        } else { None };
                        let mut result = search::search_with_session(&store,&query,options.sem,options.rerank,cache,session,&mut trace);
                        if let (Some(error), Ok(set)) = (preparation_error, &mut result) {
                            set.coverage["semantic_preparation_error"] = error.to_string().into();
                        }
                        let mut event = crate::diagnostics::RequestEvent::new(context.clone(), crate::diagnostics::Operation::Search, if result.is_ok() { crate::diagnostics::Outcome::Success } else { crate::diagnostics::Outcome::Failure });
                        event.stage = crate::diagnostics::EventStage::Server;
                        event.retrieval = Some(trace);
                        if options.diagnostics != "off" {
                            if let Some(queue) = log_queue { queue.record(event); }
                            else { crate::diagnostics::best_effort_record(cache, emission::diagnostics_mode(options), event); }
                        }
                        result?
                    }}
                }
                _=>unreachable!(),
            };
            let id=results::save(&mut store,set)?;
            results::page(&store,&id,0,options.limit,&page_budget)
        }
        _ => bail!("invalid_command: unknown command {verb}; use search for queries"),
    }
}

fn ensure_semantic_check_arity(options: &Arguments) -> Result<()> {
    ensure!(
        options.words.len() == 4,
        "usage: semantic-check MODEL_DIRECTORY cuda DEVICE_ORDINAL"
    );
    Ok(())
}
