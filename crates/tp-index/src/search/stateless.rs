//! Bounded current-source discovery that does not open result or index caches.
//! A deadline, cap, or path-first `kind:file` page limits coverage; the page
//! stays successful and its coverage names the limit and a narrower follow-up.
mod coverage;
mod current_map;
mod current_refs;
mod current_walk;
mod handles;
mod literal_prefilter;
mod read_cutoff;
mod source_reads;
mod stateless_scan;
#[cfg(test)]
mod tests;

use crate::{
    results::ResultSet,
    search::{InvocationDirectory, Query},
};
use anyhow::Result;
use read_cutoff::ReadCutoff;
use serde_json::Value;
use std::path::Path;
use tp_core::{output::OutputBudget, query_deadline::QueryDeadline};

/// Caller choices that shape one cache-free search.
#[derive(Clone, Copy, Debug)]
pub struct CurrentSearchOptions {
    /// Page size; `kind:file` text skips body reads when path matches fill it.
    pub page_limit: usize,
    pub semantic_requested: bool,
    pub rerank_requested: bool,
}

/// Searches one already-selected repository using only its current source bytes.
///
/// Semantic and rerank lanes stay disabled; the coverage object names those
/// omissions when requested or when the query is otherwise eligible.
pub fn search(
    root: &Path,
    cache_exclusion: &Path,
    query: &Query,
    origin: &InvocationDirectory,
    deadline: &QueryDeadline,
    options: CurrentSearchOptions,
) -> Result<ResultSet> {
    let cutoff = ReadCutoff::for_deadline(deadline);
    stateless_scan::search_root(
        root,
        cache_exclusion,
        query,
        origin,
        deadline,
        options,
        cutoff,
    )
}

/// Occurrence sites of `refs` words (`NAME [file:P] [lang:L] [kind:K]`) from
/// current bytes, extracting only files that contain the name.
pub fn refs(
    root: &Path,
    cache_exclusion: &Path,
    text: &str,
    origin: &InvocationDirectory,
    deadline: &QueryDeadline,
) -> Result<ResultSet> {
    let cutoff = ReadCutoff::for_deadline(deadline);
    current_refs::refs_root(root, cache_exclusion, text, origin, deadline, cutoff)
}

/// Outlines one current file, or the current files under one directory.
pub fn map_path(
    root: &Path,
    cache_exclusion: &Path,
    path: &str,
    deadline: &QueryDeadline,
) -> Result<ResultSet> {
    let cutoff = ReadCutoff::for_deadline(deadline);
    current_map::map_current_path(root, cache_exclusion, path, deadline, cutoff)
}

/// Renders one stateless page with caller-owned provenance metadata.
pub fn render_with_metadata(
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
pub fn render_members(
    parts: &[(&Path, Option<&str>, &ResultSet)],
    metadata: &Value,
    limit: usize,
    budget: &OutputBudget,
) -> Result<String> {
    crate::results::stateless_page_members(parts, metadata, limit, budget)
}
