//! Current-source workspace lookup with configured ownership and no persisted results.

use super::{WorkspaceConfig, hashed_member_cache, member_root::MemberRoot};
use crate::coded_error::CodedContext;
use crate::workspace::workspace_discovery::WorkspaceMembers;
use crate::{
    cli::Arguments,
    output::OutputBudget,
    path_codec::encode_path,
    query_deadline::QueryDeadline,
    results::ResultSet,
    search::{InvocationDirectory, Query, stateless},
    store::{is_index_cache_unavailable, preflight_results_cache},
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};

pub(super) fn eligible(options: &Arguments) -> bool {
    matches!(
        options.words.first().map(String::as_str),
        Some("search" | "map")
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
        if crate::source::acquisition::cursor_matches_root(target, &member.root)? {
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
        .with_coded_context(|| coded!(Usage, "search QUERY | map FILE"))?;
    let text = scope.words[1..].join(" ");
    ensure!(
        !text.starts_with("refs:"),
        coded!(CacheFreeUnsupported, "refs requires an index")
    );
    ensure!(
        verb != "map" || scope.words.len() == 2,
        coded!(Usage, "--no-cache map FILE")
    );
    let query = Query::parse(&text)?;
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
            if verb == "map" {
                stateless::map_file(&member.root, &exclusion, &text, deadline)
            } else {
                let origin = InvocationDirectory::within(&member.root, &invocation);
                stateless::search(
                    &member.root,
                    &exclusion,
                    &query,
                    &origin,
                    deadline,
                    options.sem,
                    options.rerank,
                )
            }
        })();
        match found {
            Ok(set) => {
                let complete =
                    !set.truncated && set.coverage.get("partial") != Some(&Value::Bool(true));
                let widen = scope.implicit_home
                    && position == 1
                    && verb == "search"
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
                if crate::coded_error::ErrorCode::TimedOut.is_code_of(&error) && answers.is_empty()
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
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    ensure!(
        !answers.is_empty(),
        coded!(
            SourceUnavailable,
            "no selected member answered current-source lookup; absence=not_proven; coverage={}",
            Value::Array(coverage.clone())
        )
    );
    let truncated = incomplete || answers.iter().any(|(_, set)| set.truncated);
    let mut metadata = json!({"workspace":config.name,"source":"current_source","cache":"unused",
        "home":roots.iter().find(|member| member.is_home).map(MemberRoot::name),
        "scope":scope_label,"coverage":coverage,"truncated":truncated});
    if let Some(fallback) = fallback {
        metadata["cache_fallback"] = fallback;
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
    let rendered = stateless::render_members(&parts, &metadata, options.limit, &budget)?;
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    Ok(rendered)
}

#[cfg(test)]
mod tests;
