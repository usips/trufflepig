//! Cache-free symbol reads retain the same selected owner as indexed `show`.

use super::{WorkspaceConfig, cache_path, hashed_member_cache, selected_member, stateless_queries};
use crate::cli::Arguments;
use anyhow::{Result, ensure};
use serde_json::json;
use tp_core::{output::OutputBudget, path_codec::encode_path, query_deadline::QueryDeadline};
use tp_index::{
    search::{InvocationDirectory, stateless},
    source,
    store::{is_index_cache_unavailable, preflight_results_cache},
};

pub(super) fn read_if_cache_free(
    config: &WorkspaceConfig,
    options: &Arguments,
    deadline: &QueryDeadline,
) -> Result<Option<String>> {
    read_with_refusal(config, options, deadline, None)
}

pub(super) fn read_after_refusal(
    config: &WorkspaceConfig,
    options: &Arguments,
    deadline: &QueryDeadline,
    error: anyhow::Error,
) -> Result<String> {
    let error = deadline.classify(error);
    if !options.is_symbol_show()
        || tp_core::coded_error::ErrorCode::TimedOut.is_anywhere_in(&error)
        || !is_index_cache_unavailable(&error)
    {
        return Err(error);
    }
    let refusal = json!({
        "status": "index_cache_unavailable",
        "reason": format!("{error:#}"),
    });
    let mut current = options.clone();
    current.no_cache = true;
    Ok(
        read_with_refusal(config, &current, deadline, Some(refusal))?
            .expect("cache-free symbol shows are handled"),
    )
}

fn read_with_refusal(
    config: &WorkspaceConfig,
    options: &Arguments,
    deadline: &QueryDeadline,
    mut refusal: Option<serde_json::Value>,
) -> Result<Option<String>> {
    if !options.is_symbol_show() {
        return Ok(None);
    }
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    ensure!(
        options.side.is_none(),
        coded!(InvalidSide, "symbol reads have no historical side")
    );
    let member = selected_member(config, options)?;
    member.verify_identity()?;
    let cache = hashed_member_cache(&member.root, options.cache.as_deref())?;
    if !options.no_cache {
        let workspace_cache = cache_path(config, options.cache.as_deref())?;
        for selected in [&workspace_cache, &cache] {
            if let Err(error) = preflight_results_cache(selected, Some(deadline)) {
                let error = deadline.classify(error);
                if !is_index_cache_unavailable(&error) {
                    return Err(error);
                }
                refusal = Some(stateless_queries::cache_failure(selected, &error));
                break;
            }
        }
        if refusal.is_none() {
            return Ok(None);
        }
    }
    let target =
        source::acquisition::show_target(&options.words).expect("symbol show has a target");
    let invocation = options.root.canonicalize()?;
    let origin = InvocationDirectory::within(&member.root, &invocation);
    let exclusion =
        crate::cli::symbol_scan_cache_exclusion(&member.root, &cache, options.cache.is_some());
    let read = stateless::show_symbol(&member.root, &exclusion, &target, &origin, deadline)
        .map_err(|error| deadline.classify(error))?;
    let mut metadata = json!({
        "workspace": config.name,
        "member": member.name(),
        "repository": encode_path(&member.root),
        "cache": "unused",
        "coverage": read.coverage,
    });
    if let Some(worktree) = &member.worktree {
        metadata["worktree"] = worktree.clone().into();
    }
    if let Some(refusal) = refusal {
        metadata["cache_fallback"] = refusal;
    }
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    let budget = OutputBudget::new(options.budget)?.with_format(options.output_format());
    let rendered = source::render_owned(read.source, &budget, &metadata)
        .map_err(|error| deadline.classify(error))?;
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    Ok(Some(rendered))
}

#[cfg(test)]
mod tests;
