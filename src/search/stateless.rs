//! Bounded current-source discovery that does not open result or index caches.
mod coverage;
mod handles;
mod scan;
#[cfg(test)]
mod tests;

use crate::{
    daemon::deadline::QueryDeadline,
    output::OutputBudget,
    results::ResultSet,
    search::{InvocationDirectory, Query},
};
use anyhow::Result;
use serde_json::Value;
use std::path::Path;

/// Searches one already-selected repository using only its current source bytes.
///
/// Semantic and rerank lanes stay disabled; the coverage object names those
/// omissions when requested or when the query is otherwise eligible.
pub(crate) fn search(
    root: &Path,
    cache_exclusion: &Path,
    query: &Query,
    origin: &InvocationDirectory,
    deadline: &QueryDeadline,
    semantic_requested: bool,
    rerank_requested: bool,
) -> Result<ResultSet> {
    scan::search_root(
        root,
        cache_exclusion,
        query,
        origin,
        deadline,
        semantic_requested,
        rerank_requested,
    )
}

/// Outlines exactly one current file without consulting a published index.
pub(crate) fn map_file(
    root: &Path,
    cache_exclusion: &Path,
    path: &str,
    deadline: &QueryDeadline,
) -> Result<ResultSet> {
    scan::map_current_file(root, cache_exclusion, path, deadline)
}

/// Renders one stateless page with caller-owned provenance metadata.
pub(crate) fn render_with_metadata(
    root: &Path,
    member: Option<&str>,
    set: &ResultSet,
    metadata: &Value,
    limit: usize,
    budget: &OutputBudget,
) -> Result<String> {
    crate::results::stateless_page(root, member, set, metadata, limit, budget)
}

/// Renders ranked member results in one page and applies one shared output budget.
pub(crate) fn render_members(
    parts: &[(&Path, Option<&str>, &ResultSet)],
    metadata: &Value,
    limit: usize,
    budget: &OutputBudget,
) -> Result<String> {
    crate::results::stateless_page_members(parts, metadata, limit, budget)
}
