//! Worktree-current answers where a parent index is behind: definitions and file
//! maps re-extracted from the worktree's bytes, never written to any index. A
//! file is changed when Git divergence lists it or its bytes no longer hash to
//! the indexed revision ([`WorktreeHashes`]).
use super::{ParentFallback, WorktreeDivergence, hit_verification::WorktreeHashes};
use crate::{
    extract::Definition,
    identity::ContentRevision,
    results::{Hit, ResultSet},
    search::{BoundPathFilter, InvocationDirectory, Query},
    source,
    store::{Store, decode_path},
};
use anyhow::{Result, ensure};
use std::collections::HashSet;

/// Most changed files re-extracted for one `sym:` answer.
const MAX_REEXTRACTED_FILES: usize = 64;

/// A parent `sym:` answer brought up to the worktree.
pub(super) struct WorktreeSymbols {
    /// Ordered by `search::rank_definitions`, as index definitions are.
    pub hits: Vec<Hit>,
    /// Every changed file found: re-extracted, gone, or unextractable (whose
    /// parent hits stay, unverified).
    pub changed: HashSet<String>,
    /// False only when every changed path and parent candidate was examined.
    pub incomplete: bool,
    /// Count of known paths that could not be re-extracted; the count is a
    /// lower bound when discovery or hashing was incomplete.
    pub unsearched_paths: usize,
    pub unsearched_paths_exact: bool,
}

/// Worktree bytes exist but their definitions cannot be known.
pub(super) fn unextractable(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.starts_with("source_excluded") || message.starts_with("extraction_unavailable")
}

impl ParentFallback {
    /// Completes a parent-index answer from worktree bytes: `sym:` re-extracts
    /// changed files, and `map` of a changed file re-extracts it. Returns the
    /// changed files it found, for the answer's differing count.
    pub(crate) fn complete_answer(
        &mut self,
        store: &Store,
        request: (&str, &[String], &Query, &InvocationDirectory),
        found: &mut ResultSet,
        hashes: &mut WorktreeHashes,
    ) -> Result<HashSet<String>> {
        let (verb, words, query, origin) = request;
        if verb == "map" {
            let Some(path) = words.get(1) else {
                return Ok(HashSet::new());
            };
            let indexed = found.hits.iter().find(|hit| &hit.path == path);
            let changed = self.divergence.contains(path)
                || indexed.is_some_and(|hit| {
                    hashes.check(&store.root, path, hit.revision.as_deref()) == Some(false)
                });
            if changed && let Some(hits) = reextracted_map(store, path) {
                found.hits = hits;
                if let Some(coverage) = found.coverage.as_object_mut() {
                    coverage.remove("no_indexed_path");
                }
                return Ok(HashSet::from([path.clone()]));
            }
        } else if verb == "search" && query.exact {
            let parent = std::mem::take(&mut found.hits);
            let symbols = self.worktree_symbols(store, query, parent, origin, hashes)?;
            found.hits = symbols.hits;
            if symbols.incomplete {
                found.truncated = true;
                found.coverage["worktree_reextraction"] = serde_json::json!({
                    "status": "incomplete",
                    "unsearched_paths": symbols.unsearched_paths,
                    "unsearched_paths_exact": symbols.unsearched_paths_exact,
                });
            }
            return Ok(symbols.changed);
        }
        Ok(HashSet::new())
    }

    /// Replaces `parent` hits in changed files with the definitions those files
    /// hold now, and adds definitions that exist only in divergent files. At most
    /// [`MAX_REEXTRACTED_FILES`] files are re-extracted, divergent files first,
    /// then parent-hit files in rank order while the hash budget lasts.
    pub(super) fn worktree_symbols(
        &mut self,
        store: &Store,
        query: &Query,
        parent: Vec<Hit>,
        origin: &InvocationDirectory,
        hashes: &mut WorktreeHashes,
    ) -> Result<WorktreeSymbols> {
        let root = &store.root;
        // A cached Git diff cannot see an edit or new file made since it was
        // recorded. Exact misses have no parent hit whose revision can reveal
        // that change, so refresh discovery only on that path.
        if query.exact && parent.is_empty() {
            self.divergence = WorktreeDivergence::refresh(
                store.index_root(),
                root,
                store.results_cache(),
                store.generation()?,
            );
        }
        let paths = query.path.bind(&store.conn).ok();
        let divergent: Vec<&str> = self
            .divergence
            .paths
            .iter()
            .filter(|path| query_admits_path(query, paths.as_ref(), path))
            .map(String::as_str)
            .collect();
        let mut changed: Vec<&str> = divergent
            .iter()
            .copied()
            .take(MAX_REEXTRACTED_FILES)
            .collect();
        let mut selected: HashSet<String> = changed.iter().map(|path| (*path).to_owned()).collect();
        let mut incomplete = !self.divergence.complete || divergent.len() > changed.len();
        let mut unsearched_paths = divergent.len().saturating_sub(changed.len());
        let mut unsearched_paths_exact = self.divergence.complete;
        let mut unsearched_changed_paths: HashSet<String> = divergent
            .iter()
            .skip(MAX_REEXTRACTED_FILES)
            .map(|path| (*path).to_owned())
            .collect();
        for hit in &parent {
            if selected.contains(&hit.path) {
                continue;
            }
            match hashes.check(root, &hit.path, hit.revision.as_deref()) {
                Some(false) if changed.len() < MAX_REEXTRACTED_FILES => {
                    selected.insert(hit.path.clone());
                    changed.push(&hit.path);
                }
                Some(false) => {
                    incomplete = true;
                    if unsearched_changed_paths.insert(hit.path.clone()) {
                        unsearched_paths += 1;
                    }
                }
                Some(true) => {}
                None => {
                    incomplete = true;
                    unsearched_paths_exact = false;
                    break;
                }
            }
        }
        let mut replaced = HashSet::with_capacity(changed.len());
        let mut unextractable_paths = HashSet::new();
        let mut current = Vec::new();
        for path in changed {
            match file_definitions(store, path, query) {
                Ok(hits) => current.extend(hits),
                Err(error) if unextractable(&error) => {
                    unextractable_paths.insert(path.to_owned());
                    incomplete = true;
                    unsearched_paths += 1;
                    continue;
                }
                Err(error) if path_missing(&error) => {}
                Err(_) => {
                    unextractable_paths.insert(path.to_owned());
                    incomplete = true;
                    unsearched_paths += 1;
                    continue;
                }
            }
            replaced.insert(path.to_owned());
        }
        let mut hits: Vec<Hit> = parent
            .into_iter()
            .filter(|hit| !replaced.contains(&hit.path))
            .chain(current)
            .collect();
        crate::search::rank_definitions(&mut hits, origin);
        replaced.extend(unextractable_paths);
        Ok(WorktreeSymbols {
            hits,
            changed: replaced,
            incomplete,
            unsearched_paths,
            unsearched_paths_exact,
        })
    }
}

/// Whether a failed read means that the path no longer exists.
fn path_missing(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    })
}

/// Every definition named by `query` in the worktree file at `path`.
fn file_definitions(store: &Store, path: &str, query: &Query) -> Result<Vec<Hit>> {
    let bytes = source::read_contained(&store.root, &decode_path(path)?, source::MAX_READ_BYTES)?;
    ensure!(
        !bytes.contains(&0),
        "source_excluded: {path} is binary; the index does not extract it"
    );
    let extraction = source::usable_extraction(path, crate::extract::extract(path, &bytes))?;
    let revision = ContentRevision::of(&bytes).to_string();
    Ok(extraction
        .definitions
        .into_iter()
        .filter(|definition| crate::search::names_definition(query, path, definition))
        .map(|definition| definition_hit(path, &bytes, &revision, definition))
        .collect())
}

/// Types and members of a changed file, extracted from its worktree bytes;
/// `None` keeps the parent's rows when the file cannot be extracted.
fn reextracted_map(store: &Store, path: &str) -> Option<Vec<Hit>> {
    let bytes = source::read_contained(
        &store.root,
        &decode_path(path).ok()?,
        source::MAX_READ_BYTES,
    )
    .ok()?;
    if bytes.contains(&0) {
        return None;
    }
    let revision = ContentRevision::of(&bytes).to_string();
    let extraction = source::usable_extraction(path, crate::extract::extract(path, &bytes));
    let mut definitions = extraction.ok()?.definitions;
    crate::search::outline_extracted(&mut definitions);
    Some(
        definitions
            .into_iter()
            .map(|definition| definition_hit(path, &bytes, &revision, definition))
            .collect(),
    )
}

fn definition_hit(path: &str, bytes: &[u8], revision: &str, definition: Definition) -> Hit {
    let (start_line, end_line) = source::line_span(bytes, definition.start, definition.end);
    Hit {
        handle: String::new(),
        path: path.to_owned(),
        revision: Some(revision.to_owned()),
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
        repeats: None,
        snippet: None,
        differs: false,
    }
}

/// Filters a changed path as the query's `file:` (bound once) and `lang:` words would.
fn query_admits_path(query: &Query, paths: Option<&BoundPathFilter>, path: &str) -> bool {
    paths.is_none_or(|paths| paths.matches(path))
        && (query.language.is_empty() || crate::extract::language(path) == query.language)
}
