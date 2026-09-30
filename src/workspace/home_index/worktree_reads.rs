//! Worktree-current answers where a parent index is behind: definitions and file
//! maps re-extracted from the worktree's bytes, never written to any index. A
//! file is changed when Git divergence lists it or its bytes no longer hash to
//! the indexed revision ([`worktree_matches`]).
use super::{ParentFallback, hit_verification::worktree_matches};
use crate::{
    extract::Definition,
    identity::ContentRevision,
    results::{Hit, ResultSet},
    search::Query,
    source,
    store::{Store, decode_path},
};
use anyhow::{Result, ensure};
use std::collections::HashSet;

/// Most changed files re-extracted for one `sym:` answer.
const MAX_REEXTRACTED_FILES: usize = 64;
/// Definition kinds a file `map` lists (types and members).
#[rustfmt::skip]
const MAP_KINDS: [&str; 22] = [
    "addon", "module", "namespace", "struct", "class", "trait", "type", "enum", "impl",
    "interface", "record", "delegate", "xenforo_class_extension", "function", "method",
    "constructor", "property", "field", "event", "constant", "enum_case", "macro",
];

/// A parent `sym:` answer brought up to the worktree.
pub(super) struct WorktreeSymbols {
    /// Ordered as `search::definitions` orders them.
    pub hits: Vec<Hit>,
    /// Changed files whose definitions are unknown (binary or failed
    /// extraction); their parent hits stay, unverified.
    pub unextractable: HashSet<String>,
}

/// Worktree bytes exist but their definitions cannot be known.
pub(super) fn unextractable(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.starts_with("source_excluded") || message.starts_with("extraction_unavailable")
}

impl ParentFallback {
    /// Completes a parent-index answer from worktree bytes: `sym:` re-extracts
    /// changed files, and `map` of a changed file re-extracts it.
    pub(in crate::workspace) fn complete_answer(
        &self,
        store: &Store,
        verb: &str,
        words: &[String],
        query: &Query,
        found: &mut ResultSet,
    ) {
        if verb == "map" {
            let changed = |path: &str| {
                self.divergence.contains(path)
                    || found
                        .hits
                        .iter()
                        .find(|hit| hit.path == path)
                        .is_some_and(|hit| {
                            !worktree_matches(&store.root, path, hit.revision.as_deref())
                        })
            };
            if let Some(path) = words.get(1)
                && changed(path)
                && let Some(hits) = reextracted_map(store, path)
            {
                found.hits = hits;
            }
        } else if verb == "search" && query.exact {
            found.hits = self
                .worktree_symbols(store, query, std::mem::take(&mut found.hits))
                .hits;
        }
    }

    /// Replaces `parent` hits in changed files with the definitions those files
    /// hold now, and adds definitions that exist only in divergent files. At most
    /// [`MAX_REEXTRACTED_FILES`] files are re-extracted, divergent files first.
    pub(super) fn worktree_symbols(
        &self,
        store: &Store,
        query: &Query,
        parent: Vec<Hit>,
    ) -> WorktreeSymbols {
        let root = &store.root;
        let mut changed: Vec<&str> = self
            .divergence
            .paths
            .iter()
            .filter(|path| query_admits_path(query, path))
            .take(MAX_REEXTRACTED_FILES)
            .map(String::as_str)
            .collect();
        let mut checked = HashSet::with_capacity(parent.len());
        for hit in &parent {
            if changed.len() >= MAX_REEXTRACTED_FILES {
                break;
            }
            if !changed.contains(&hit.path.as_str())
                && checked.insert(hit.path.as_str())
                && !worktree_matches(root, &hit.path, hit.revision.as_deref())
            {
                changed.push(&hit.path);
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
                    continue;
                }
                // A missing or unreadable file no longer defines anything.
                Err(_) => {}
            }
            replaced.insert(path.to_owned());
        }
        let mut hits: Vec<Hit> = parent
            .into_iter()
            .filter(|hit| !replaced.contains(&hit.path))
            .chain(current)
            .collect();
        crate::search::rank_definitions(&mut hits);
        WorktreeSymbols {
            hits,
            unextractable: unextractable_paths,
        }
    }
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
        .filter(|definition| {
            definition.name == query.text
                && (query.kind.is_empty() || definition.kind == query.kind)
        })
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
    definitions.retain(|definition| MAP_KINDS.contains(&definition.kind.as_str()));
    definitions.sort_by_key(|definition| definition.start);
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
        snippet: None,
    }
}

/// Filters a changed path as the query's `file:` and `lang:` words would.
fn query_admits_path(query: &Query, path: &str) -> bool {
    path.starts_with(&query.path)
        && (query.language.is_empty() || crate::extract::language(path) == query.language)
}
