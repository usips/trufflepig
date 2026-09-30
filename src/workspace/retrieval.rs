//! Fair rank merging of independent published member snapshots.
#[cfg(test)]
mod tests;

use super::{
    WorkspaceConfig, coordinator, member_cache,
    member_root::MemberRoot,
    result_cache::{MemberSnapshot, OwnedEntry, WorkspaceResults, WorkspaceSet},
};
use crate::{
    cli::Arguments,
    daemon::deadline::{QueryDeadline, TIMED_OUT, is_timed_out},
    diagnostics::{DiagnosticsMode, EventStage, Operation, Outcome, RequestContext, RequestEvent},
    output::OutputBudget,
    results::ResultEntry,
    search::{self, Query, telemetry::RetrievalTrace},
    semantic::SemanticSession,
    store::{Store, is_index_warming},
};
use anyhow::{Context, Result, ensure};
use serde_json::json;

/// How long a query waits for the home member's first publication, typically a
/// freshly seeded worktree, before answering from the rest of the workspace.
const HOME_PUBLICATION_WAIT: std::time::Duration = std::time::Duration::from_secs(8);

/// Opens a member's published index for one query; the home member waits up to
/// [`HOME_PUBLICATION_WAIT`] (capped by `deadline`) for its first publication.
fn open_member_read(
    member: &MemberRoot,
    member_cache: &std::path::Path,
    await_home: bool,
    deadline: QueryDeadline,
) -> Result<Store> {
    let wait = std::time::Instant::now() + deadline.cap(HOME_PUBLICATION_WAIT);
    loop {
        match Store::open_read(&member.root, member_cache, deadline) {
            Err(error)
                if await_home
                    && member.is_home
                    && is_index_warming(&error)
                    && std::time::Instant::now() < wait =>
            {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            opened => return opened,
        }
    }
}

/// Members to search. Without a selector, only the home member is searched when
/// one exists (`implicit_home`); `search` widens to all members when home has no
/// hits or no published index.
struct Scope {
    members: Vec<usize>,
    words: Vec<String>,
    implicit_home: bool,
}

fn scope(roots: &[MemberRoot], options: &Arguments) -> Result<Scope> {
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
    let home = roots
        .iter()
        .find(|root| root.is_home)
        .map(|root| root.name());
    let implicit_home = selection.is_none() && home.is_some();
    let selected = match selection.as_deref() {
        Some("ws:home") => Some(home.context("member_required: ws:home has no home member")?),
        Some("ws:all") => None,
        None => home,
        Some(s) => Some(s.strip_prefix("in:").expect("validated selector")),
    };
    if let Some(name) = selected {
        ensure!(
            roots.iter().any(|m| m.name() == name),
            "invalid_member: member is not configured"
        );
    }
    let mut members: Vec<_> = roots
        .iter()
        .enumerate()
        .filter(|(_, m)| selected.is_none_or(|n| m.name() == n))
        .map(|(i, _)| i)
        .collect();
    members.sort_by_key(|&i| (Some(roots[i].name()) != home, roots[i].name()));
    Ok(Scope {
        members,
        words,
        implicit_home,
    })
}
pub(super) fn search(
    config: &WorkspaceConfig,
    cache: &std::path::Path,
    results: &WorkspaceResults,
    options: &Arguments,
    context: &RequestContext,
    session: &mut SemanticSession,
    mut deadline: QueryDeadline,
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
            if options.no_daemon {
                deadline.pause_during(|| Store::open(&member.root, &member_cache)?.index())?;
            }
            let store = open_member_read(member, &member_cache, !options.no_daemon, deadline)?;
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
                    &member_cache,
                    semantic_query,
                    reranker,
                    &mut trace,
                )?;
                if query.exact {
                    // `sym:` namesakes nearest the invocation directory come first.
                    let invocation = options.root.canonicalize()?;
                    let origin = search::InvocationDirectory::within(&member.root, &invocation);
                    search::rank_declarations(&mut found.hits, &origin);
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
            let owner = MemberSnapshot::capture(member, &store, &member_cache, found.generation)?;
            Ok::<_, anyhow::Error>((owner, found))
        })()
        .map_err(|error| deadline.classify(error));
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
        event.member = Some(member.name().to_owned());
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
                let mut coverage = json!({"member":member.name(),"state":"searched","generation":owner.generation,"retained":entries.len(),"truncated":truncated,"partial":partial_coverage(&found.coverage),"unsearched":unsearched_files(&found.coverage),"issues":coverage_issues(&found.coverage)});
                if let Some(label) = &member.worktree {
                    coverage["root"] = crate::store::encode_path(&member.root).into();
                    coverage["worktree"] = label.clone().into();
                }
                for key in search::REFERENCE_COVERAGE_KEYS {
                    if let Some(value) = found.coverage.get(key) {
                        coverage[key] = value.clone();
                    }
                }
                set.coverage.push(coverage);
                set.truncated |= truncated;
                set.owners.push(owner);
                lists.push(entries.into_iter());
            }
            Err(error) => {
                let state = if is_index_warming(&error) {
                    "warming"
                } else if is_timed_out(&error) {
                    "timed_out"
                } else {
                    "unavailable"
                };
                let mut coverage = json!({"member":member.name(),"state":state});
                if let Some(label) = &member.worktree {
                    coverage["root"] = crate::store::encode_path(&member.root).into();
                    coverage["worktree"] = label.clone().into();
                }
                set.coverage.push(coverage);
            }
        }
        if implicit_home && position == 1 {
            let others = roots.len() - 1;
            let home_empty = lists.first().is_none_or(|list| list.len() == 0);
            set.scope = Some(if others == 0 {
                "home".to_owned()
            } else if home_empty {
                queue.extend((0..roots.len()).filter(|&i| i != index));
                let reason = if lists.is_empty() {
                    "home unavailable"
                } else {
                    "no home hits"
                };
                format!("all ({reason})")
            } else {
                format!(
                    "home (ws:all adds {others} member{})",
                    if others == 1 { "" } else { "s" }
                )
            });
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

/// Counters of files whose bytes a search could not examine. Excluded (binary,
/// oversized) files, parse failures (still lexically searchable), and semantic
/// lane status do not make lexical coverage partial; they stay in `issues`.
const UNSEARCHED_KEYS: [&str; 4] = [
    "walk_failures",
    "truncated_files",
    "live_read_failures",
    "live_walk_failures",
];

fn partial_coverage(coverage: &serde_json::Value) -> bool {
    UNSEARCHED_KEYS
        .iter()
        .any(|key| coverage[key].as_u64().unwrap_or(0) > 0)
}

/// Files a search could not examine, for the `partial (N unsearched)` summary.
fn unsearched_files(coverage: &serde_json::Value) -> u64 {
    UNSEARCHED_KEYS
        .iter()
        .map(|key| coverage[key].as_u64().unwrap_or(0))
        .sum()
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
