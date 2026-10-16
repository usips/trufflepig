//! Re-run ordinary search lanes against bounded current-file snapshots.
use super::{Query, search_prepared, telemetry::RetrievalTrace};
use crate::{
    daemon::deadline::QueryDeadline,
    extract::Extraction,
    identity::ContentRevision,
    results::Hit,
    source,
    store::{Store, current_source},
};
use anyhow::Result;

/// Runs the normal exact, occurrence, lexical, and filename lanes on one current file.
pub(crate) fn search_file(
    parent: &Store,
    query: &Query,
    path: &str,
    bytes: &[u8],
    extraction: &Extraction,
    deadline: &QueryDeadline,
) -> Result<(Vec<Hit>, bool)> {
    let current = Store::unpublished(&parent.root, parent.results_cache())?
        .with_query_deadline(deadline.clone());
    current_source::insert(&current, path, bytes, extraction)?;
    let mut trace = RetrievalTrace::disabled();
    let result = search_prepared(
        &current,
        query,
        parent.results_cache(),
        None,
        None,
        &mut trace,
        deadline,
    )?;
    Ok((result.hits, result.truncated))
}

/// A current-source placeholder for an old semantic candidate whose vector is stale.
pub(crate) fn semantic_representative(
    query: &Query,
    path: &str,
    bytes: &[u8],
    extraction: &Extraction,
) -> Option<Hit> {
    let definition = if query.kind.is_empty() || query.kind == "file" {
        None
    } else {
        Some(
            extraction
                .definitions
                .iter()
                .find(|definition| definition.kind == query.kind)?,
        )
    };
    let revision = ContentRevision::of(bytes).to_string();
    let (start, end, name, kind, container) = definition.map_or_else(
        || (0, 0, path.to_owned(), "file".to_owned(), None),
        |definition| {
            (
                definition.start,
                definition.end,
                definition.name.clone(),
                definition.kind.clone(),
                definition.container.clone(),
            )
        },
    );
    let (start_line, end_line) = source::line_span(bytes, start, end);
    let snippet = super::snippets::preview(
        bytes,
        start,
        end,
        start_line,
        &super::snippets::preview_terms(&query.text),
        &name,
    );
    Some(Hit {
        handle: String::new(),
        path: path.to_owned(),
        revision: Some(revision),
        start,
        end,
        start_line,
        end_line,
        name,
        kind,
        container,
        provenance: Some("semantic_unverified".into()),
        resolution: None,
        candidates: Vec::new(),
        target: None,
        repeats: None,
        snippet,
        differs: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_representative_previews_current_file_bytes() {
        let bytes = b"pub struct CurrentSemanticName;\n";
        let extraction = crate::extract::extract("src/lib.rs", bytes);
        let query = Query::parse("CurrentSemanticName").unwrap();
        let hit = semantic_representative(&query, "src/lib.rs", bytes, &extraction).unwrap();
        let revision = ContentRevision::of(bytes).to_string();

        assert_eq!(hit.revision.as_deref(), Some(revision.as_str()));
        assert!(
            hit.snippet
                .as_ref()
                .is_some_and(|snippet| snippet.text.contains("CurrentSemanticName"))
        );
    }
}
