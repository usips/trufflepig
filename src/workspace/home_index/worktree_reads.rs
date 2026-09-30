//! Worktree-current answers where a parent index is behind: definitions and file
//! maps re-extracted from the worktree's bytes (`source::reextract_definition`,
//! `extract::extract`), never written to any index.
use super::ParentFallback;
use crate::{
    identity::ContentRevision,
    results::{Hit, ResultSet},
    search::Query,
    source,
    store::{Store, decode_path},
};

/// Most changed files searched for a `sym:` name the parent index lacks.
pub(super) const MAX_REEXTRACTED_FILES: usize = 64;
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
    pub(super) fn worktree_definitions(&self, store: &Store, query: &Query) -> Vec<Hit> {
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
        if bytes.contains(&0) {
            return None;
        }
        let revision = ContentRevision::of(&bytes).to_string();
        // Unknown definitions keep the parent's map, flagged `differs`.
        let extraction = source::usable_extraction(path, crate::extract::extract(path, &bytes));
        let mut definitions = extraction.ok()?.definitions;
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
