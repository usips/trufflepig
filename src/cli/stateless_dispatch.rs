//! Local current-source retrieval without persistent cache access.

use super::Arguments;
use anyhow::{Result, ensure};
use std::path::Path;
use tp_core::coded_error::CodedContext;
use tp_core::{output::OutputBudget, query_deadline::QueryDeadline};
use tp_index::search::{InvocationDirectory, Query, stateless};

pub(super) fn eligible(options: &Arguments) -> bool {
    matches!(
        options.words.first().map(String::as_str),
        Some("search" | "map" | "refs")
    )
}

pub(super) fn run(
    root: &Path,
    cache: &Path,
    options: &Arguments,
    deadline: &QueryDeadline,
) -> Result<String> {
    run_with_metadata(root, cache, options, deadline, &serde_json::json!({}))
}

pub(super) fn run_fallback(
    root: &Path,
    cache: &Path,
    options: &Arguments,
    deadline: &QueryDeadline,
    error: &anyhow::Error,
) -> Result<String> {
    run_with_metadata(
        root,
        cache,
        options,
        deadline,
        &serde_json::json!({
            "cache_fallback": {"status":"index_cache_unavailable", "cache":cache,
                "reason":error.chain().last().map(ToString::to_string)}
        }),
    )
}

fn run_with_metadata(
    root: &Path,
    cache: &Path,
    options: &Arguments,
    deadline: &QueryDeadline,
    metadata: &serde_json::Value,
) -> Result<String> {
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    let text = options.words.get(1..).unwrap_or_default().join(" ");
    let invocation = options.root.canonicalize()?;
    let origin = InvocationDirectory::within(root, &invocation);
    let set = match options.words.first().map(String::as_str) {
        Some("search") if text.starts_with("refs:") => {
            stateless::refs(root, cache, &text, &origin, deadline)?
        }
        Some("search") => {
            let query = Query::parse(&text)?;
            stateless::search(
                root,
                cache,
                &query,
                &origin,
                deadline,
                stateless::CurrentSearchOptions {
                    page_limit: options.limit,
                    semantic_requested: options.sem,
                    rerank_requested: options.rerank,
                },
            )?
        }
        Some("refs") => stateless::refs(root, cache, &text, &origin, deadline)?,
        Some("map") => {
            ensure!(
                options.words.len() == 2,
                coded!(Usage, "--no-cache map PATH")
            );
            stateless::map_path(
                root,
                cache,
                options
                    .words
                    .get(1)
                    .with_coded_context(|| coded!(Usage, "map PATH"))?,
                deadline,
            )?
        }
        _ => anyhow::bail!(coded!(
            CacheFreeUnsupported,
            "use search, refs NAME, or map PATH"
        )),
    };
    // A scan that ran into its deadline already returned partial coverage; only
    // a cancelled caller discards the finished page.
    ensure!(
        !deadline.cancelled(),
        coded!(TimedOut, "query was cancelled")
    );
    let budget = OutputBudget::new(options.budget)?.with_format(options.output_format());
    stateless::render_with_metadata(root, None, &set, metadata, options.limit, &budget)
}

#[cfg(test)]
mod tests;
