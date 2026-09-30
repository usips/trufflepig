//! Fair rank merging of independent published member snapshots.
mod member_coverage;
mod member_scope;
#[cfg(test)]
mod tests;

use super::{
    WorkspaceConfig, coordinator,
    home_index::{
        DifferingFiles, HomeIndexPolicy, HomeIndexSource, ParentFallback, WorktreeHashes,
        resolve_home_index,
    },
    member_cache,
    result_cache::{MemberSnapshot, OwnedEntry, WorkspaceResults, WorkspaceSet},
};
use crate::{
    cli::Arguments,
    daemon::deadline::{QueryDeadline, TIMED_OUT, is_timed_out},
    diagnostics::{DiagnosticsMode, EventStage, Operation, Outcome, RequestContext, RequestEvent},
    output::OutputBudget,
    results::{ResultEntry, ResultSet},
    search::{self, Query, telemetry::RetrievalTrace},
    semantic::SemanticSession,
    store::{Store, is_index_warming},
};
use anyhow::{Context, Result, ensure};
use member_coverage::{fact_limit_paths, member_row, searched_row, short_reason};
use member_scope::{Scope, implicit_home_scope, scope};

/// One member's answer: its hits, or warming without a published index.
enum MemberAnswer {
    Found {
        owner: Box<MemberSnapshot>,
        found: ResultSet,
        /// A parent-index answer and the files among its hits that differ.
        fallback: Option<(ParentFallback, DifferingFiles)>,
    },
    Warming(Option<&'static str>),
}

/// Runs a workspace query: `--no-daemon` indexes members inline; otherwise
/// members answer from published indexes ([`HomeIndexPolicy::for_search`]).
pub(super) fn search(
    config: &WorkspaceConfig,
    cache: &std::path::Path,
    results: &WorkspaceResults,
    options: &Arguments,
    context: &RequestContext,
    session: &mut SemanticSession,
    deadline: QueryDeadline,
) -> Result<String> {
    let policy = HomeIndexPolicy::for_search(options.no_daemon);
    search_with_policy(
        config, cache, results, options, context, session, deadline, policy,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn search_with_policy(
    config: &WorkspaceConfig,
    cache: &std::path::Path,
    results: &WorkspaceResults,
    options: &Arguments,
    context: &RequestContext,
    session: &mut SemanticSession,
    mut deadline: QueryDeadline,
    policy: HomeIndexPolicy,
) -> Result<String> {
    let roots = config.member_roots(&options.root.canonicalize()?)?;
    let Scope {
        members: mut queue,
        words,
        implicit_home,
    } = scope(&roots, options)?;
    let log_cache = super::diagnostic_location(options)
        .map(|(_, cache, _, _)| cache)
        .unwrap_or_else(|_| cache.to_path_buf());
    let verb = words
        .first()
        .map(String::as_str)
        .context("usage: retrieval command is required")?;
    let text = words[1..].join(" ");
    let query = Query::parse(&text)?;
    let semantic_eligible_shape = !query.exact
        && !query.regex
        && !matches!(verb, "refs" | "map")
        && !text.starts_with("refs:");
    let semantic = options.sem && semantic_eligible_shape;
    let rerank = options.rerank && semantic_eligible_shape;
    let started = std::time::Instant::now();
    session.set_no_daemon(options.no_daemon);
    let (prepared, semantic_error) = match session.prepare(semantic, cache, &query.text) {
        Ok(prepared) => (prepared, None),
        // Keep the cause chain: the outer context alone hides why the worker failed.
        Err(error) => (None, Some(format!("{error:#}"))),
    };
    let preparation_us = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
    let mut preparation_recorded = false;
    let mut set = WorkspaceSet {
        workspace: config.name.clone(),
        home: roots
            .iter()
            .find(|root| root.is_home)
            .map(|root| root.name().to_owned()),
        owners: Vec::with_capacity(roots.len()),
        coverage: Vec::with_capacity(roots.len()),
        hits: Vec::new(),
        truncated: false,
        scope: None,
    };
    let mut lists = Vec::with_capacity(roots.len());
    let mut map_miss = None;
    let mut position = 0;
    while position < queue.len() {
        let index = queue[position];
        position += 1;
        // A member cannot reserve the full workspace staging capacity before others run.
        let byte_quota = crate::results::MAX_BYTES / queue.len();
        let hit_quota = crate::results::MAX_HITS / queue.len();
        let member = &roots[index];
        let member_cache = member_cache(member, options.cache.as_deref())?;
        let mode = match options.diagnostics.as_str() {
            "off" => DiagnosticsMode::Off,
            "detailed" => DiagnosticsMode::Detailed,
            _ => DiagnosticsMode::Metadata,
        };
        let mut trace = if matches!(mode, DiagnosticsMode::Off) {
            RetrievalTrace::disabled()
        } else {
            RetrievalTrace::default()
        };
        let started = std::time::Instant::now();
        let found = (|| {
            ensure!(!deadline.expired(), "{TIMED_OUT}: query deadline expired");
            member.verify_identity()?;
            coordinator::ensure_member(member, options)?;
            if matches!(policy, HomeIndexPolicy::IndexInline) {
                deadline.pause_during(|| Store::open(&member.root, &member_cache)?.index())?;
            }
            let explicit_cache = options.cache.as_deref();
            let (store, fallback) =
                match resolve_home_index(member, &member_cache, explicit_cache, policy, deadline) {
                    HomeIndexSource::Own(store) => (store, None),
                    HomeIndexSource::Parent(view) => (view.store, Some(view.fallback)),
                    HomeIndexSource::Warming { reason } => {
                        return Ok(MemberAnswer::Warming(reason));
                    }
                    HomeIndexSource::Unavailable { reason } => anyhow::bail!(reason),
                };
            let origin =
                search::InvocationDirectory::within(&member.root, &options.root.canonicalize()?);
            let preparation_error = if semantic && !options.no_daemon {
                crate::semantic::preparation::schedule(&member.root, &member_cache).err()
            } else {
                None
            };
            let mut found = if verb == "refs" || text.starts_with("refs:") {
                search::references(&store, &search::reference_query(&text)?)?
            } else if verb == "map" {
                let found = search::map(&store, words.get(1).map(String::as_str).unwrap_or(""))?;
                if let Some(miss) = search::map_miss(&found) {
                    map_miss.get_or_insert_with(|| miss.to_owned());
                }
                found
            } else {
                let semantic_query = prepared.clone();
                let reranker = rerank.then_some(&*session as &dyn search::RerankScorer);
                let mut found = search::search_prepared(
                    &store,
                    &query,
                    store.index_cache(),
                    semantic_query,
                    reranker,
                    &mut trace,
                )?;
                if query.exact {
                    // `sym:` namesakes nearest the invocation directory come first.
                    search::rank_definitions(&mut found.hits, &origin);
                }
                found
            };
            if let Some(error) = &semantic_error {
                found.coverage["semantic_status"] = "unavailable".into();
                found.coverage["semantic_reason"] = error.clone().into();
            }
            if let Some(error) = preparation_error {
                found.coverage["semantic_preparation_error"] = error.to_string().into();
            }
            let fallback = fallback.map(|fallback| {
                let mut hashes = WorktreeHashes::default();
                let request = (verb, words.as_slice(), &query, &origin);
                let changed = fallback.complete_answer(&store, request, &mut found, &mut hashes);
                let differing = fallback.check_hits(&store.root, &found.hits, &mut hashes, changed);
                (fallback, differing)
            });
            if found.coverage["truncated_files"].as_u64().unwrap_or(0) > 0 {
                found.coverage["unsearched_paths"] = fact_limit_paths(&store)?.into();
            }
            let owner = MemberSnapshot::capture(member, &store, &member_cache, found.generation)?;
            Ok::<_, anyhow::Error>(MemberAnswer::Found {
                owner: Box::new(owner),
                found,
                fallback,
            })
        })()
        .map_err(|error| deadline.classify(error));
        if semantic && !preparation_recorded {
            trace.query_preparation_us = Some(preparation_us);
            preparation_recorded = true;
        }
        let mut event = RequestEvent::new(
            context.clone(),
            Operation::Search,
            if matches!(found, Ok(MemberAnswer::Found { .. })) {
                Outcome::Success
            } else {
                Outcome::Unavailable
            },
        );
        event.stage = EventStage::Server;
        event.elapsed_micros = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
        event.retrieval = Some(trace);
        event.workspace = Some(config.id.clone());
        event.member = Some(member.name().to_owned());
        crate::diagnostics::best_effort_record(&log_cache, mode, event);
        let mut member_state = "searched";
        match found {
            Ok(MemberAnswer::Found {
                mut owner,
                found,
                fallback,
            }) => {
                owner.coverage = found.coverage.clone();
                let owner_index = set.owners.len();
                let mut bytes = 0;
                let mut entries = Vec::with_capacity(found.hits.len().min(hit_quota));
                let total = found.hits.len();
                for (rank, hit) in found.hits.into_iter().enumerate().take(hit_quota) {
                    let entry = OwnedEntry {
                        owner: owner_index,
                        member_rank: rank + 1,
                        worktree_differs: fallback.as_ref().is_some_and(|(_, d)| d.flags(&hit)),
                        entry: ResultEntry::LiveSource(hit),
                    };
                    bytes += serde_json::to_vec(&entry)?.len();
                    if bytes > byte_quota {
                        break;
                    }
                    entries.push(entry);
                }
                let truncated = found.truncated || entries.len() < total;
                let mut coverage = searched_row(
                    member,
                    owner.generation,
                    entries.len(),
                    truncated,
                    &found.coverage,
                );
                if let Some((fallback, differing_files)) = &fallback {
                    let differing = entries.iter().filter(|e| e.worktree_differs).count();
                    fallback.describe(&mut coverage, differing_files, differing);
                }
                for key in search::REFERENCE_COVERAGE_KEYS {
                    if let Some(value) = found.coverage.get(key) {
                        coverage[key] = value.clone();
                    }
                }
                set.coverage.push(coverage);
                set.truncated |= truncated;
                set.owners.push(*owner);
                lists.push(entries.into_iter());
            }
            Ok(MemberAnswer::Warming(reason)) => {
                member_state = "warming";
                let mut coverage = member_row(member, member_state);
                if let Some(reason) = reason {
                    coverage["reason"] = reason.into();
                }
                set.coverage.push(coverage);
            }
            Err(error) => {
                member_state = if is_index_warming(&error) {
                    "warming"
                } else if is_timed_out(&error) {
                    "timed_out"
                } else {
                    "unavailable"
                };
                let mut coverage = member_row(member, member_state);
                if member_state == "unavailable" {
                    coverage["reason"] = short_reason(&error).into();
                }
                set.coverage.push(coverage);
            }
        }
        if implicit_home && position == 1 {
            let home_hits = lists.first().is_some_and(|list| list.len() > 0);
            let (scope, widen) =
                implicit_home_scope(roots.len() - 1, !lists.is_empty(), home_hits, member_state);
            if widen {
                queue.extend((0..roots.len()).filter(|&i| i != index));
            }
            set.scope = Some(scope);
        }
    }
    if lists.iter().all(|list| list.len() == 0)
        && let Some(miss) = map_miss
    {
        anyhow::bail!(miss);
    }
    ensure!(
        !set.owners.is_empty()
            || !set
                .coverage
                .iter()
                .any(|member| member["state"] == "timed_out"),
        "{TIMED_OUT}: query deadline expired before any member answered"
    );
    // An unselected query whose home is not answering reports that on an empty page.
    ensure!(
        !set.owners.is_empty() || implicit_home,
        "workspace_unavailable: no selected member has an available published index; inspect ws status or use --no-daemon"
    );
    set.hits.reserve(lists.iter().map(|list| list.len()).sum());
    loop {
        let before = set.hits.len();
        for list in &mut lists {
            set.hits.extend(list.next());
        }
        if set.hits.len() == before {
            break;
        }
    }
    let id = results.save(set)?;
    let budget = OutputBudget::new(options.budget)?.with_format(options.output_format());
    results.page(&id, 0, options.limit, &budget)
}
