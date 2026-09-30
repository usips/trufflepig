//! Worktree-current answers where a parent index is behind: definitions and file
//! maps re-extracted from the worktree's bytes (`source::reextract_definition`,
//! `extract::extract`), never written to any index.
use super::{
    HomeIndexPolicy, HomeIndexSource, ParentFallback, ParentIndexView, resolve_home_index,
};
use crate::{
    daemon::deadline::QueryDeadline,
    identity::ContentRevision,
    results::{self, Hit, ResultEntry, ResultSet},
    search::Query,
    source::{self, AcquiredSource, SourceSide, acquisition},
    store::{Store, decode_path},
    workspace::member_root::MemberRoot,
};
use anyhow::{Result, ensure};
use std::path::Path;

/// Most changed files searched for a `sym:` name the parent index lacks.
const MAX_REEXTRACTED_FILES: usize = 64;
/// Other same-named definitions listed after a re-extracted `sym:` read.
const SYMBOL_ALTERNATIVES: usize = 5;
/// Definition kinds a file `map` lists (types and members).
const MAP_KINDS: [&str; 22] = [
    "addon",
    "module",
    "namespace",
    "struct",
    "class",
    "trait",
    "type",
    "enum",
    "impl",
    "interface",
    "record",
    "delegate",
    "xenforo_class_extension",
    "function",
    "method",
    "constructor",
    "property",
    "field",
    "event",
    "constant",
    "enum_case",
    "macro",
];

impl ParentFallback {
    /// Completes a parent-index answer from worktree bytes: a `sym:` search with
    /// no hit looks in changed files, and `map` of a changed file re-extracts it.
    pub(in crate::workspace) fn complete_answer(
        &self,
        store: &Store,
        verb: &str,
        words: &[String],
        query: &Query,
        found: &mut ResultSet,
    ) {
        if verb == "map" {
            if let Some(path) = words.get(1)
                && let Some(hits) = self.reextracted_map(store, path)
            {
                found.hits = hits;
            }
        } else if verb == "search" && query.exact && found.hits.is_empty() {
            found.hits = self.worktree_definitions(store, query);
        }
    }

    /// Definitions named by `query` in up to [`MAX_REEXTRACTED_FILES`] changed files.
    fn worktree_definitions(&self, store: &Store, query: &Query) -> Vec<Hit> {
        self.divergence
            .paths
            .iter()
            .filter(|path| query_admits_path(query, path))
            .take(MAX_REEXTRACTED_FILES)
            .filter_map(|path| {
                source::reextract_definition(&store.root, path, &query.text, &query.kind, 0)
                    .ok()
                    .flatten()
            })
            .map(|definition| definition.hit())
            .collect()
    }

    /// Types and members of a changed file, extracted from its worktree bytes.
    fn reextracted_map(&self, store: &Store, path: &str) -> Option<Vec<Hit>> {
        if !self.differs(path) {
            return None;
        }
        let bytes = source::read_contained(
            &store.root,
            &decode_path(path).ok()?,
            source::MAX_READ_BYTES,
        )
        .ok()?;
        let revision = ContentRevision::of(&bytes).to_string();
        let mut definitions = crate::extract::extract(path, &bytes).definitions;
        definitions.retain(|definition| MAP_KINDS.contains(&definition.kind.as_str()));
        definitions.sort_by_key(|definition| definition.start);
        Some(
            definitions
                .into_iter()
                .map(|definition| {
                    let (start_line, end_line) =
                        source::line_span(&bytes, definition.start, definition.end);
                    Hit {
                        handle: String::new(),
                        path: path.to_owned(),
                        revision: Some(revision.clone()),
                        start: definition.start,
                        end: definition.end,
                        start_line,
                        end_line,
                        name: definition.name,
                        kind: definition.kind,
                        container: definition.container,
                        provenance: Some("reextracted".into()),
                        resolution: None,
                        candidates: Vec::new(),
                        target: None,
                        snippet: None,
                    }
                })
                .collect(),
        )
    }
}

/// Filters a changed path as the query's `file:` and `lang:` words would.
fn query_admits_path(query: &Query, path: &str) -> bool {
    path.starts_with(&query.path)
        && (query.language.is_empty() || crate::extract::language(path) == query.language)
}

/// Provenance of a definition re-extracted from worktree bytes because the
/// parent index that answered is behind them.
pub(in crate::workspace) fn reextracted_from(member: &str) -> String {
    format!("{member} index; re-extracted in worktree")
}

/// `show TARGET` on a member without a result handle: through its own index,
/// through its parent's while a worktree warms ([`acquire_through_parent`]), or,
/// with neither published, from current bytes for explicit path reads. The flag
/// reports a re-extracted definition.
pub(in crate::workspace) fn acquire_home_read(
    member: &MemberRoot,
    cache: &Path,
    explicit_cache: Option<&Path>,
    target: &str,
    side: Option<SourceSide>,
    deadline: QueryDeadline,
) -> Result<(Store, AcquiredSource, bool)> {
    let policy = HomeIndexPolicy::AwaitDaemon(std::time::Duration::ZERO);
    match resolve_home_index(member, cache, explicit_cache, policy, deadline) {
        HomeIndexSource::Own(store) => {
            let source = acquisition::acquire(&store, target, side)?;
            Ok((store, source, false))
        }
        HomeIndexSource::Parent(view) => {
            let (source, reextracted) = acquire_through_parent(&view, target, side)?;
            Ok((view.store, source, reextracted))
        }
        HomeIndexSource::Warming { reason } => {
            ensure!(
                !target.starts_with("sym:"),
                "index_warming: {} has no published index yet{}; `show path:FILE:A-B` reads current bytes",
                member.display_name(),
                reason
                    .map(|reason| format!(" ({reason})"))
                    .unwrap_or_default()
            );
            let store = Store::open(&member.root, cache)?;
            let source = acquisition::acquire(&store, target, side)?;
            Ok((store, source, false))
        }
        HomeIndexSource::Unavailable { reason } => anyhow::bail!(reason),
    }
}

/// `show TARGET` through a parent index. A `sym:` read whose definition lives in
/// a changed file, or exists only in the worktree, is re-extracted from worktree
/// bytes; the flag reports that.
pub(in crate::workspace) fn acquire_through_parent(
    view: &ParentIndexView,
    target: &str,
    side: Option<SourceSide>,
) -> Result<(AcquiredSource, bool)> {
    let error = match acquisition::acquire(&view.store, target, side) {
        Ok(source) => return Ok((source, false)),
        Err(error) => error,
    };
    let message = error.to_string();
    if !target.starts_with("sym:")
        || !(message.starts_with("stale_source") || message.starts_with("no_definition"))
    {
        return Err(error);
    }
    let query = Query::parse(target)?;
    let found = crate::search::definitions(&view.store, &query)?;
    let fallback = &view.fallback;
    let mut hits = Vec::with_capacity(found.hits.len());
    for (rank, hit) in found.hits.into_iter().enumerate() {
        // An incomplete divergence cannot clear a file, so leading hits re-extract.
        let changed = fallback.differs(&hit.path)
            || (!fallback.divergence.complete && rank < MAX_REEXTRACTED_FILES);
        if !changed {
            hits.push(hit);
        } else if let Ok(Some(current)) = source::reextract_definition(
            &view.store.root,
            &hit.path,
            &hit.name,
            &hit.kind,
            hit.start_line,
        ) {
            hits.push(current.hit());
        }
    }
    if hits.is_empty() {
        hits = fallback.worktree_definitions(&view.store, &query);
    }
    if hits.is_empty() {
        return Err(error);
    }
    let total = hits.len();
    let alternatives = hits[1..]
        .iter()
        .take(SYMBOL_ALTERNATIVES)
        .map(|hit| {
            format!(
                "{}:{}-{} {}",
                hit.path, hit.start_line, hit.end_line, hit.kind
            )
        })
        .collect();
    let first = ResultEntry::LiveSource(hits[0].clone());
    let set = results::save_entries(
        &view.store,
        found.generation,
        found.coverage,
        hits.into_iter().map(ResultEntry::LiveSource).collect(),
        found.truncated,
    )?;
    let mut source = acquisition::acquire_entry(&view.store, &format!("{set}:1"), first, None)?;
    source.definitions = Some((total, alternatives));
    Ok((source, true))
}

/// Re-reads a parent-index definition hit whose worktree file changed: the
/// same-name, same-kind definition nearest its recorded line, from current bytes.
pub(in crate::workspace) fn reextracted_entry(
    store: &Store,
    handle: &str,
    entry: &ResultEntry,
) -> Result<Option<AcquiredSource>> {
    let ResultEntry::LiveSource(hit) = entry else {
        return Ok(None);
    };
    if matches!(hit.kind.as_str(), "region" | "file" | "source_read") {
        return Ok(None);
    }
    let Some(current) =
        source::reextract_definition(&store.root, &hit.path, &hit.name, &hit.kind, hit.start_line)?
    else {
        return Ok(None);
    };
    Ok(Some(AcquiredSource {
        bytes: current.bytes,
        path: current.path,
        revision: current.revision,
        span: current.span,
        verified: true,
        handle: handle.to_owned(),
        side: None,
        historical: None,
        definitions: None,
    }))
}
