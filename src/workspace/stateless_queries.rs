//! Current-source workspace lookup with configured ownership and no persisted results.

use super::{WorkspaceConfig, hashed_member_cache, member_root::MemberRoot};
use crate::cli::Arguments;
use crate::workspace::workspace_discovery::WorkspaceMembers;
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::time::Duration;
use tp_core::coded_error::CodedContext;
use tp_core::{output::OutputBudget, path_codec::encode_path, query_deadline::QueryDeadline};
use tp_index::{
    results::ResultSet,
    search::{InvocationDirectory, Query, stateless},
    store::{is_index_cache_unavailable, preflight_results_cache},
};

const ROUTER_REPLACEMENT_SOURCE_BUDGET: Duration = Duration::from_secs(4);

pub(super) struct RouterReplacementAnswer {
    pub output: String,
    pub complete: bool,
    pub incomplete_reason: Option<String>,
}

pub(super) fn eligible(options: &Arguments) -> bool {
    matches!(
        options.words.first().map(String::as_str),
        Some("search" | "map" | "refs")
    )
}

pub(super) fn cursor_owner(
    config: &WorkspaceConfig,
    options: &Arguments,
    target: &str,
    deadline: &QueryDeadline,
) -> Result<MemberRoot> {
    let roots = config.member_roots(&options.root.canonicalize()?)?;
    let mut owner = None;
    for member in roots {
        ensure!(
            !deadline.expired(),
            coded!(TimedOut, "query deadline expired")
        );
        if options
            .member
            .as_ref()
            .is_some_and(|name| name != member.name())
        {
            continue;
        }
        if member.verify_identity().is_err() {
            continue;
        }
        if tp_index::source::acquisition::cursor_matches_root(target, &member.root)? {
            ensure!(
                owner.is_none(),
                coded!(InvalidCursor, "ambiguous configured source owner")
            );
            owner = Some(member);
        }
    }
    owner.with_coded_context(|| {
        coded!(
            InvalidCursor,
            "source owner is not a verified selected workspace member"
        )
    })
}

pub(super) fn cache_refused(
    config: &WorkspaceConfig,
    options: &Arguments,
    deadline: &QueryDeadline,
) -> Result<Option<Value>> {
    let roots = config.member_roots(&options.root.canonicalize()?)?;
    let scope = super::retrieval::member_scope::scope(&roots, options)?;
    let cache = super::cache_path(config, options.cache.as_deref())?;
    let mut caches = Vec::with_capacity(scope.members.len() + 1);
    caches.push(cache);
    for index in scope.members {
        caches.push(hashed_member_cache(
            &roots[index].root,
            options.cache.as_deref(),
        )?);
    }
    for cache in caches {
        if let Err(error) = preflight_results_cache(&cache, Some(deadline)) {
            let error = deadline.classify(error);
            return if is_index_cache_unavailable(&error) {
                Ok(Some(cache_failure(&cache, &error)))
            } else {
                Err(error)
            };
        }
    }
    Ok(None)
}

pub(super) fn cache_failure(cache: &std::path::Path, error: &anyhow::Error) -> Value {
    json!({"status":"index_cache_unavailable","cache":encode_path(cache),"reason":error.chain().last().map(ToString::to_string)})
}

pub(super) fn run(
    config: &WorkspaceConfig,
    options: &Arguments,
    deadline: &QueryDeadline,
    fallback: Option<Value>,
) -> Result<String> {
    Ok(run_query(config, options, deadline, fallback, None)?.output)
}

/// Repeats an indexed workspace read with bounded current-source coverage.
pub(super) fn run_after_router_replacement(
    config: &WorkspaceConfig,
    options: &Arguments,
    deadline: &QueryDeadline,
    failed_member: &MemberRoot,
) -> Result<RouterReplacementAnswer> {
    let deadline = deadline.capped(ROUTER_REPLACEMENT_SOURCE_BUDGET);
    run_query(config, options, &deadline, None, Some(failed_member.name()))
}

fn run_query(
    config: &WorkspaceConfig,
    options: &Arguments,
    deadline: &QueryDeadline,
    fallback: Option<Value>,
    router_replaced_member: Option<&str>,
) -> Result<RouterReplacementAnswer> {
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    let invocation = options.root.canonicalize()?;
    let roots = config.member_roots(&invocation)?;
    let scope = super::retrieval::member_scope::scope(&roots, options)?;
    let verb = scope
        .words
        .first()
        .with_coded_context(|| coded!(Usage, "search QUERY | refs NAME | map PATH"))?;
    let text = scope.words[1..].join(" ");
    let references = verb == "refs" || (verb == "search" && text.starts_with("refs:"));
    ensure!(
        verb != "map" || scope.words.len() == 2,
        coded!(Usage, "--no-cache map PATH")
    );
    let query = if references || verb == "map" {
        Query::default()
    } else {
        Query::parse(&text)?
    };
    let mut queue = scope.members;
    let mut answers: Vec<(usize, ResultSet)> = Vec::with_capacity(roots.len());
    let mut coverage = Vec::with_capacity(roots.len());
    let mut position = 0;
    let mut scope_label = if scope.implicit_home {
        "home"
    } else {
        "selected"
    };
    let mut incomplete = false;
    let mut source_coverage_complete = true;
    while position < queue.len() {
        let index = queue[position];
        position += 1;
        let member = &roots[index];
        let found = (|| {
            ensure!(
                !deadline.expired(),
                coded!(TimedOut, "query deadline expired")
            );
            member.verify_identity()?;
            let exclusion = hashed_member_cache(&member.root, options.cache.as_deref())?;
            let origin = InvocationDirectory::within(&member.root, &invocation);
            if verb == "map" {
                stateless::map_path(&member.root, &exclusion, &text, deadline)
            } else if references {
                stateless::refs(&member.root, &exclusion, &text, &origin, deadline)
            } else {
                stateless::search(
                    &member.root,
                    &exclusion,
                    &query,
                    &origin,
                    deadline,
                    stateless::CurrentSearchOptions {
                        page_limit: options.limit,
                        semantic_requested: options.sem,
                        rerank_requested: options.rerank,
                    },
                )
            }
        })();
        match found {
            Ok(set) => {
                source_coverage_complete &= !set.truncated
                    && set.coverage["current_source"]["complete"] == true
                    && set.coverage["absence"] != "not_proven"
                    && set.coverage["partial"] != true;
                let complete =
                    !set.truncated && set.coverage.get("partial") != Some(&Value::Bool(true));
                let widen = scope.implicit_home
                    && position == 1
                    && verb != "map"
                    && set.hits.is_empty()
                    && complete;
                let mut row = json!({"member":member.name(),"root":encode_path(&member.root),
                    "state":"searched","source":"current_source","coverage":set.coverage,
                    "truncated":set.truncated,"hits":set.hits.len()});
                if let Some(worktree) = &member.worktree {
                    row["worktree"] = worktree.clone().into();
                }
                coverage.push(row);
                answers.push((index, set));
                if widen {
                    queue.extend((0..roots.len()).filter(|other| *other != index));
                    scope_label = "all (no home hits)";
                }
            }
            Err(error) => {
                source_coverage_complete = false;
                if tp_core::coded_error::ErrorCode::TimedOut.is_code_of(&error)
                    && answers.is_empty()
                {
                    return Err(error);
                }
                incomplete = true;
                coverage.push(
                    json!({"member":member.name(),"root":encode_path(&member.root),
                    "state":"unavailable","reason":format!("{error:#}"),"absence":"not_proven"}),
                );
            }
        }
    }
    // Members that ran into the deadline already returned partial coverage; only
    // a cancelled caller discards the finished answers.
    ensure!(
        !deadline.cancelled(),
        coded!(TimedOut, "query was cancelled")
    );
    ensure!(
        !answers.is_empty(),
        coded!(
            SourceUnavailable,
            "no selected member answered current-source lookup; absence=not_proven; coverage={}",
            Value::Array(coverage.clone())
        )
    );
    let complete = source_coverage_complete && !incomplete;
    let truncated = incomplete || answers.iter().any(|(_, set)| set.truncated);
    let incomplete_reason = (!complete).then(|| current_source_incomplete_reason(&coverage));
    let mut metadata = json!({"workspace":config.name,"source":"current_source","cache":"unused",
        "home":roots.iter().find(|member| member.is_home).map(MemberRoot::name),
        "scope":scope_label,"coverage":coverage,"truncated":truncated});
    if let Some(fallback) = fallback {
        metadata["cache_fallback"] = fallback;
    }
    if let Some(member) = router_replaced_member {
        metadata["router_fallback"] = json!({
            "cause": "router_binary_replaced",
            "member": member,
            "mode": "no_cache",
            "coverage": if complete { "complete" } else { "partial" },
        });
    }
    let parts: Vec<_> = answers
        .iter()
        .map(|(index, set)| {
            (
                roots[*index].root.as_path(),
                Some(roots[*index].name()),
                set,
            )
        })
        .collect();
    let budget = OutputBudget::new(options.budget)?.with_format(options.output_format());
    let output = stateless::render_members(&parts, &metadata, options.limit, &budget)?;
    Ok(RouterReplacementAnswer {
        output,
        complete,
        incomplete_reason,
    })
}

fn current_source_incomplete_reason(coverage: &[Value]) -> String {
    let row = coverage
        .iter()
        .find(|row| row["state"] == "unavailable")
        .or_else(|| {
            coverage.iter().find(|row| {
                row["coverage"]["current_source"]["complete"] == false
                    || row["coverage"]["absence"] == "not_proven"
            })
        });
    let Some(row) = row else {
        return "current-source coverage incomplete".to_owned();
    };
    let member = row["member"].as_str().unwrap_or("unknown member");
    let reason = row["reason"]
        .as_str()
        .or_else(|| row["coverage"]["current_source"]["limit"].as_str())
        .unwrap_or("current-source coverage incomplete");
    let mut summary: String = format!("{member}: {reason}").chars().take(120).collect();
    if format!("{member}: {reason}").chars().count() > 120 {
        summary.push('…');
    }
    summary
}

#[cfg(test)]
mod tests;
