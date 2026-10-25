//! Local current-source retrieval without persistent cache access.

use super::Arguments;
use crate::coded_error::CodedContext;
use crate::{
    output::OutputBudget,
    query_deadline::QueryDeadline,
    search::{InvocationDirectory, Query, stateless},
};
use anyhow::{Result, ensure};
use std::path::Path;

pub(super) fn eligible(options: &Arguments) -> bool {
    matches!(
        options.words.first().map(String::as_str),
        Some("search" | "map")
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
    let set = match options.words.first().map(String::as_str) {
        Some("search") => {
            let text = options.words[1..].join(" ");
            ensure!(
                !text.starts_with("refs:"),
                coded!(CacheFreeUnsupported, "refs requires an index")
            );
            let query = Query::parse(&text)?;
            let invocation = options.root.canonicalize()?;
            let origin = InvocationDirectory::within(root, &invocation);
            stateless::search(
                root,
                cache,
                &query,
                &origin,
                deadline,
                options.sem,
                options.rerank,
            )?
        }
        Some("map") => {
            ensure!(
                options.words.len() == 2,
                coded!(Usage, "--no-cache map FILE")
            );
            stateless::map_file(
                root,
                cache,
                options
                    .words
                    .get(1)
                    .with_coded_context(|| coded!(Usage, "map FILE"))?,
                deadline,
            )?
        }
        _ => anyhow::bail!(coded!(CacheFreeUnsupported, "use search or map FILE")),
    };
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    let budget = OutputBudget::new(options.budget)?.with_format(options.output_format());
    let rendered =
        stateless::render_with_metadata(root, None, &set, metadata, options.limit, &budget)?;
    ensure!(
        !deadline.expired(),
        coded!(TimedOut, "query deadline expired")
    );
    Ok(rendered)
}

#[cfg(test)]
mod tests;
