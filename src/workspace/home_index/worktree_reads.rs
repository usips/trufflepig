//! Worktree-current answers where a parent index is behind: definitions and file
//! maps re-extracted from the worktree's bytes, never written to any index. A
//! file is changed when Git divergence lists it or its bytes no longer hash to
//! the indexed revision ([`WorktreeHashes`]).
use super::{ParentFallback, WorktreeDivergence, hit_verification::WorktreeHashes};
use crate::{
    daemon::deadline::QueryDeadline,
    extract::Definition,
    identity::ContentRevision,
    results::{Hit, ResultSet},
    search::{BoundPathFilter, InvocationDirectory, Query},
    source,
    store::{Store, decode_path},
};
use anyhow::{Result, ensure};
use rusqlite::OptionalExtension;
use std::collections::HashSet;

/// Most changed files re-evaluated for one search query.
const MAX_RECHECKED_FILES: usize = 64;

/// Plain text and `sym:` queries can return stale indexed source candidates.
fn rechecks_search_query(query: &Query) -> bool {
    query.exact || (!query.regex && !query.text.is_empty())
}

/// A parent search answer brought up to the worktree.
pub(super) struct WorktreeSymbols {
    /// Current-source matches mixed with unchanged parent candidates.
    pub hits: Vec<Hit>,
    /// Every changed file found: rechecked, gone, or unextractable (whose
    /// parent hits stay, unverified).
    pub changed: HashSet<String>,
    /// False only when every changed path and parent candidate was examined.
    pub incomplete: bool,
    /// Count of known paths that could not be re-extracted; the count is a
    /// lower bound when discovery or hashing was incomplete.
    pub unsearched_paths: usize,
    pub unsearched_paths_exact: bool,
    /// At least one current-file retrieval lane reached its candidate limit.
    pub truncated: bool,
}

/// Worktree bytes exist but their definitions cannot be known.
pub(super) fn unextractable(error: &anyhow::Error) -> bool {
    use crate::coded_error::{ErrorCode, error_code};
    matches!(
        error_code(error),
        Some(ErrorCode::SourceExcluded | ErrorCode::ExtractionUnavailable)
    )
}

impl ParentFallback {
    /// Completes a parent-index answer from worktree bytes: searches recheck
    /// changed files, and `map` of a changed file re-extracts it.
    /// Returns the changed files it found, for the answer's differing count.
    pub(crate) fn complete_answer(
        &mut self,
        store: &Store,
        request: (&str, &[String], &Query, &InvocationDirectory),
        found: &mut ResultSet,
        hashes: &mut WorktreeHashes,
        deadline: &QueryDeadline,
    ) -> Result<HashSet<String>> {
        check_deadline(deadline)?;
        let (verb, words, query, origin) = request;
        if verb == "map" {
            let Some(target) = words.get(1) else {
                return Ok(HashSet::new());
            };
            let filter = crate::search::PathFilter::prefix(target);
            let Some(path) = filter
                .resolve_single_file(&store.conn)?
                .or_else(|| filter.includes().first().cloned())
            else {
                return Ok(HashSet::new());
            };
            let git_changed = self.divergence.contains(&path);
            let bytes_changed = Self::refresh_current_map_with_hashes(store, &path, found, hashes)?;
            if git_changed && !bytes_changed {
                Self::replace_map_from_current_source(store, &path, found);
            }
            if git_changed || bytes_changed {
                check_deadline(deadline)?;
                return Ok(HashSet::from([path.clone()]));
            }
        } else if verb == "search" && rechecks_search_query(query) {
            let parent = std::mem::take(&mut found.hits);
            let symbols = self.worktree_symbols(store, query, parent, origin, hashes, deadline)?;
            found.hits = symbols.hits;
            found.truncated |= symbols.truncated;
            if !symbols.changed.is_empty() && found.coverage["rerank_status"] == "ready" {
                found.coverage["rerank_status"] = "unavailable".into();
                found.coverage["rerank_reason"] =
                    "current-source replacements were not reranked".into();
                if let Some(coverage) = found.coverage.as_object_mut() {
                    coverage.remove("rerank_window");
                }
            }
            if symbols.incomplete {
                found.truncated = true;
                found.coverage["worktree_reextraction"] = serde_json::json!({
                    "status": "incomplete",
                    "unsearched_paths": symbols.unsearched_paths,
                    "unsearched_paths_exact": symbols.unsearched_paths_exact,
                });
            }
            if !symbols.changed.is_empty()
                && matches!(
                    found.coverage["semantic_status"].as_str(),
                    Some("ready" | "partial")
                )
            {
                found.coverage["worktree_semantic"] = serde_json::json!({
                    "status": "unverified",
                    "files": symbols.changed.len(),
                });
                found.coverage["semantic_status"] = "partial".into();
            }
            return Ok(symbols.changed);
        }
        Ok(HashSet::new())
    }

    /// Replaces an exact-file outline when its current bytes differ from the
    /// indexed revision, including a readable worktree index published before
    /// a later commit.
    pub(crate) fn refresh_current_map(
        store: &Store,
        target: &str,
        found: &mut ResultSet,
    ) -> Result<bool> {
        let Some(path) =
            crate::search::PathFilter::prefix(target).resolve_single_file(&store.conn)?
        else {
            return Ok(false);
        };
        let mut hashes = WorktreeHashes::default();
        Self::refresh_current_map_with_hashes(store, &path, found, &mut hashes)
    }

    /// Retains indexed hits only when current bytes verify their revision.
    pub(crate) fn refresh_current_map_with_hashes(
        store: &Store,
        path: &str,
        found: &mut ResultSet,
        hashes: &mut WorktreeHashes,
    ) -> Result<bool> {
        let revision: Option<String> = store
            .conn
            .query_row("SELECT revision FROM files WHERE path=?1", [path], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(revision) = revision else {
            if found.hits.iter().any(|hit| hit.path == path) {
                Self::mark_map_unavailable(found, path, "indexed map revision could not be read");
                return Ok(true);
            }
            return Ok(false);
        };
        match hashes.check(&store.root, path, Some(&revision)) {
            Some(true) => Ok(false),
            Some(false) => {
                Self::replace_map_from_current_source(store, path, found);
                Ok(true)
            }
            None => {
                Self::mark_map_unavailable(found, path, "current source could not be verified");
                Ok(true)
            }
        }
    }

    fn replace_map_from_current_source(store: &Store, path: &str, found: &mut ResultSet) {
        if let Some(hits) = reextracted_map(store, path) {
            found.hits = hits;
            if let Some(coverage) = found.coverage.as_object_mut() {
                coverage.remove("no_indexed_path");
            }
            found.coverage["worktree_map"] = serde_json::json!({
                "status": "reextracted",
                "path": path,
            });
        } else {
            Self::mark_map_unavailable(found, path, "changed source could not be outlined");
        }
    }

    fn mark_map_unavailable(found: &mut ResultSet, path: &str, reason: &str) {
        found.hits.clear();
        found.truncated = true;
        found.coverage["worktree_map"] = serde_json::json!({
            "status": "unavailable",
            "path": path,
            "reason": reason,
        });
    }

    /// Replaces `parent` hits in changed files with candidates those files hold
    /// now. At most [`MAX_RECHECKED_FILES`] files are rechecked, divergent files first,
    /// then parent-hit files in rank order while the hash budget lasts.
    pub(super) fn worktree_symbols(
        &mut self,
        store: &Store,
        query: &Query,
        parent: Vec<Hit>,
        origin: &InvocationDirectory,
        hashes: &mut WorktreeHashes,
        deadline: &QueryDeadline,
    ) -> Result<WorktreeSymbols> {
        check_deadline(deadline)?;
        let root = &store.root;
        // A cached Git diff cannot see an edit or new file made since it was
        // recorded. Search misses have no parent hit whose revision can reveal
        // a recent addition, so refresh discovery on that path.
        if rechecks_search_query(query) && parent.is_empty() {
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
            .take(MAX_RECHECKED_FILES)
            .collect();
        let mut selected: HashSet<String> = changed.iter().map(|path| (*path).to_owned()).collect();
        let mut incomplete = !self.divergence.complete || divergent.len() > changed.len();
        let mut unsearched_paths = divergent.len().saturating_sub(changed.len());
        let mut unsearched_paths_exact = self.divergence.complete;
        let mut unsearched_changed_paths: HashSet<String> = divergent
            .iter()
            .skip(MAX_RECHECKED_FILES)
            .map(|path| (*path).to_owned())
            .collect();
        for hit in &parent {
            if selected.contains(&hit.path) {
                continue;
            }
            match hashes.check(root, &hit.path, hit.revision.as_deref()) {
                Some(false) if changed.len() < MAX_RECHECKED_FILES => {
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
        let changed_order: Vec<String> = changed.iter().map(|path| (*path).to_owned()).collect();
        let mut replaced = HashSet::with_capacity(changed.len());
        let mut unextractable_paths = HashSet::new();
        let mut current_by_path = std::collections::HashMap::with_capacity(changed.len());
        let mut truncated = false;
        for path in changed {
            check_deadline(deadline)?;
            match current_file_hits(store, path, query, &parent, deadline) {
                Ok((hits, file_truncated)) => {
                    truncated |= file_truncated;
                    current_by_path.insert(path.to_owned(), hits);
                }
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
        let mut emitted = HashSet::with_capacity(replaced.len());
        let mut hits = Vec::with_capacity(parent.len());
        for hit in parent {
            if replaced.contains(&hit.path) {
                if emitted.insert(hit.path.clone()) {
                    hits.extend(current_by_path.remove(&hit.path).unwrap_or_default());
                }
            } else {
                hits.push(hit);
            }
        }
        for path in changed_order {
            if replaced.contains(&path) && emitted.insert(path.clone()) {
                hits.extend(current_by_path.remove(&path).unwrap_or_default());
            }
        }
        if query.exact {
            crate::search::rank_definitions(&mut hits, origin);
        }
        replaced.extend(unextractable_paths);
        Ok(WorktreeSymbols {
            hits,
            changed: replaced,
            incomplete,
            unsearched_paths,
            unsearched_paths_exact,
            truncated,
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

/// Re-runs normal search lanes against one changed file's worktree bytes.
fn current_file_hits(
    store: &Store,
    path: &str,
    query: &Query,
    parent: &[Hit],
    deadline: &QueryDeadline,
) -> Result<(Vec<Hit>, bool)> {
    check_deadline(deadline)?;
    let bytes = source::read_contained(&store.root, &decode_path(path)?, source::MAX_READ_BYTES)?;
    ensure!(
        !bytes.contains(&0),
        coded!(
            SourceExcluded,
            "{path} is binary; the index does not extract it"
        )
    );
    let extraction = source::usable_extraction(path, crate::extract::extract(path, &bytes))?;
    check_deadline(deadline)?;
    let (mut hits, truncated) =
        crate::search::search_current_file(store, query, path, &bytes, &extraction, deadline)?;
    let semantic = parent
        .iter()
        .any(|hit| hit.path == path && hit.provenance.as_deref() == Some("semantic"));
    if semantic
        && let Some(hit) = crate::search::semantic_representative(query, path, &bytes, &extraction)
    {
        hits.push(hit);
    }
    for hit in &mut hits {
        if let Some(provenance) = &mut hit.provenance {
            *provenance = format!("worktree_current:{provenance}");
        }
    }
    Ok((hits, truncated))
}

fn check_deadline(deadline: &QueryDeadline) -> Result<()> {
    if deadline.expired() {
        anyhow::bail!(coded!(TimedOut, "worktree re-extraction deadline expired"));
    }
    Ok(())
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
