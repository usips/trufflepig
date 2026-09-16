//! Fair rank merging of independent published member snapshots.
#[cfg(test)]
mod tests;

use super::{
    WorkspaceConfig, coordinator, member_cache,
    result_cache::{MemberSnapshot, OwnedEntry, WorkspaceResults, WorkspaceSet},
};
use crate::{
    cli::Arguments,
    diagnostics::{DiagnosticsMode, EventStage, Operation, Outcome, RequestContext, RequestEvent},
    output::OutputBudget,
    results::ResultEntry,
    search::{self, Query, telemetry::RetrievalTrace},
    semantic::SemanticSession,
    store::Store,
};
use anyhow::{Context, Result, ensure};
use serde_json::json;

fn scope(config: &WorkspaceConfig, options: &Arguments) -> Result<(Vec<usize>, Vec<String>)> {
    let mut selection = options.member.clone().map(|m| format!("in:{m}"));
    let mut words = Vec::with_capacity(options.words.len());
    for argument in &options.words {
        let mut remaining = String::new();
        for chunk in argument.split_inclusive(char::is_whitespace) {
            let word = chunk.trim_end();
            if word.starts_with("in:") || matches!(word, "ws:home" | "ws:all") {
                ensure!(
                    selection.as_ref().is_none_or(|old| old == word),
                    "invalid_scope: conflicting workspace selectors"
                );
                selection = Some(word.to_owned());
            } else {
                remaining.push_str(chunk);
            }
        }
        if !remaining.trim().is_empty() {
            words.push(remaining.trim_end().to_owned());
        }
    }
    let home = config
        .home(&options.root.canonicalize()?)
        .map(|m| m.name.as_str());
    let selected = match selection.as_deref() {
        Some("ws:home") => Some(home.context("member_required: ws:home has no home member")?),
        Some("ws:all") | None => None,
        Some(s) => Some(s.strip_prefix("in:").expect("validated selector")),
    };
    if let Some(name) = selected {
        ensure!(
            config.members.iter().any(|m| m.name == name),
            "invalid_member: member is not configured"
        );
    }
    let mut members: Vec<_> = config
        .members
        .iter()
        .enumerate()
        .filter(|(_, m)| selected.is_none_or(|n| m.name == n))
        .map(|(i, _)| i)
        .collect();
    members.sort_by_key(|&i| {
        (
            Some(config.members[i].name.as_str()) != home,
            &config.members[i].name,
        )
    });
    Ok((members, words))
}
pub(super) fn search(
    config: &WorkspaceConfig,
    cache: &std::path::Path,
    results: &WorkspaceResults,
    options: &Arguments,
    context: &RequestContext,
    session: &mut SemanticSession,
) -> Result<String> {
    let (selected, words) = scope(config, options)?;
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
        Err(error) => (None, Some(error.to_string())),
    };
    let preparation_us = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
    let mut preparation_recorded = false;
    let mut set = WorkspaceSet {
        workspace: config.name.clone(),
        home: config
            .home(&options.root.canonicalize()?)
            .map(|m| m.name.clone()),
        owners: Vec::with_capacity(selected.len()),
        coverage: Vec::with_capacity(selected.len()),
        hits: Vec::new(),
        truncated: false,
    };
    let mut lists = Vec::with_capacity(selected.len());
    // A member cannot reserve the full workspace staging capacity before others run.
    let byte_quota = crate::results::MAX_BYTES / selected.len();
    let hit_quota = crate::results::MAX_HITS / selected.len();
    for index in selected {
        let member = &config.members[index];
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
            member.verify_identity()?;
            coordinator::ensure_member(member, options)?;
            let mut store = Store::open(&member.root, &member_cache)?;
            if options.no_daemon {
                store.index()?;
            }
            ensure!(
                store.generation()? > 0,
                "member_warming: initial index is not published"
            );
            let preparation_error = if semantic && !options.no_daemon {
                crate::semantic::preparation::schedule(&member.root, &member_cache).err()
            } else {
                None
            };
            let mut found = if verb == "refs" || text.starts_with("refs:") {
                search::references(
                    &store,
                    if verb == "refs" {
                        words.get(1).context("usage: refs NAME")?
                    } else {
                        text.strip_prefix("refs:").expect("prefix")
                    },
                )?
            } else if verb == "map" {
                search::map(&store, words.get(1).map(String::as_str).unwrap_or(""))?
            } else {
                let semantic_query = prepared.clone();
                let reranker = rerank.then_some(&*session as &dyn search::RerankScorer);
                search::search_prepared(
                    &store,
                    &query,
                    &member_cache,
                    semantic_query,
                    reranker,
                    &mut trace,
                )?
            };
            if let Some(error) = &semantic_error {
                found.coverage["semantic_status"] = "unavailable".into();
                found.coverage["semantic_reason"] = error.clone().into();
            }
            if let Some(error) = preparation_error {
                found.coverage["semantic_preparation_error"] = error.to_string().into();
            }
            let owner = MemberSnapshot::capture(member, &store, &member_cache, found.generation)?;
            Ok::<_, anyhow::Error>((owner, found))
        })();
        if semantic && !preparation_recorded {
            trace.query_preparation_us = Some(preparation_us);
            preparation_recorded = true;
        }
        let mut event = RequestEvent::new(
            context.clone(),
            Operation::Search,
            if found.is_ok() {
                Outcome::Success
            } else {
                Outcome::Unavailable
            },
        );
        event.stage = EventStage::Server;
        event.elapsed_micros = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
        event.retrieval = Some(trace);
        event.workspace = Some(config.id.clone());
        event.member = Some(member.name.clone());
        crate::diagnostics::best_effort_record(&log_cache, mode, event);
        match found {
            Ok((mut owner, found)) => {
                owner.coverage = found.coverage.clone();
                let owner_index = set.owners.len();
                let mut bytes = 0;
                let mut entries = Vec::with_capacity(found.hits.len().min(hit_quota));
                let total = found.hits.len();
                for (rank, hit) in found.hits.into_iter().enumerate().take(hit_quota) {
                    let entry = OwnedEntry {
                        owner: owner_index,
                        member_rank: rank + 1,
                        entry: ResultEntry::LiveSource(hit),
                    };
                    bytes += serde_json::to_vec(&entry)?.len();
                    if bytes > byte_quota {
                        break;
                    }
                    entries.push(entry);
                }
                let truncated = found.truncated || entries.len() < total;
                set.coverage.push(json!({"member":member.name,"state":"searched","generation":owner.generation,"retained":entries.len(),"truncated":truncated,"partial":partial_coverage(&found.coverage),"issues":coverage_issues(&found.coverage)}));
                set.truncated |= truncated;
                set.owners.push(owner);
                lists.push(entries.into_iter());
            }
            Err(error) => {
                let state = if error.to_string().starts_with("member_warming:") {
                    "warming"
                } else {
                    "unavailable"
                };
                set.coverage
                    .push(json!({"member":member.name,"state":state}));
            }
        }
    }
    ensure!(
        !set.owners.is_empty(),
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

fn partial_coverage(coverage: &serde_json::Value) -> bool {
    coverage.get("semantic_preparation_error").is_some()
        || matches!(
            coverage["semantic_status"].as_str(),
            Some("unavailable" | "partial")
        )
        || coverage["rerank_status"].as_str() == Some("unavailable")
        || [
            "parse_failures",
            "excluded_files",
            "walk_failures",
            "truncated_files",
            "live_read_failures",
            "live_walk_failures",
            "semantic_failures",
            "semantic_pending",
        ]
        .iter()
        .any(|key| coverage[key].as_u64().unwrap_or(0) > 0)
}

fn coverage_issues(coverage: &serde_json::Value) -> serde_json::Value {
    let mut issues = serde_json::Map::new();
    for key in [
        "parse_failures",
        "excluded_files",
        "walk_failures",
        "truncated_files",
        "live_read_failures",
        "live_walk_failures",
        "semantic_failures",
        "semantic_pending",
    ] {
        if coverage[key].as_u64().unwrap_or(0) > 0 {
            issues.insert(key.into(), coverage[key].clone());
        }
    }
    for key in [
        "semantic_status",
        "semantic_reason",
        "semantic_preparation_error",
        "rerank_status",
        "rerank_reason",
    ] {
        if let Some(value) = coverage.get(key) {
            issues.insert(key.into(), value.clone());
        }
    }
    if coverage.get("semantic_total_regions").is_some() {
        for key in [
            "semantic_total_regions",
            "semantic_regions",
            "semantic_scope",
        ] {
            issues.insert(key.into(), coverage[key].clone());
        }
    }
    serde_json::Value::Object(issues)
}
