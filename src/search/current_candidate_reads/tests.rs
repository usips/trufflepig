use crate::{
    output::OutputBudget,
    results,
    search::{self, Query},
    source,
};

#[test]
fn published_candidates_use_current_names_references_and_text() {
    let old = b"pub struct ColumnImpoundOverflow;\npub fn use_column() { let _ = ColumnImpoundOverflow; let _ = RetainedName; }\n// overflow reference marker\n";
    let (root, cache, store) = search::tests::fixture(&[("lib.rs", old)]);
    let query = Query::parse("ColumnImpoundOverflow lang:rust file:lib.rs").unwrap();
    let initial = search::search(&store, &query, false, cache.path()).unwrap();
    assert!(!initial.hits.is_empty());
    let current = b"// moved\npub struct ColumnReceiptOverflow;\npub fn use_column() { let _ = ColumnReceiptOverflow; let _ = RetainedName; }\n// overflow reference marker now current\n";
    std::fs::write(root.path().join("lib.rs"), current).unwrap();
    let stale = search::search(&store, &query, false, cache.path()).unwrap();
    assert!(stale.hits.is_empty(), "{:?}", stale.hits);
    assert!(!stale.truncated);
    for query in ["RetainedName file:lib.rs", "overflow reference marker"] {
        let result =
            search::search(&store, &Query::parse(query).unwrap(), false, cache.path()).unwrap();
        assert!(!result.hits.is_empty(), "{query}");
        assert!(result.hits.iter().all(|hit| hit.revision.as_deref()
            == Some(&crate::identity::ContentRevision::of(current).to_string())));
        let id = results::save(&store, result).unwrap();
        let shown = source::show(
            &store,
            &format!("{id}:1"),
            &OutputBudget::new(4000).unwrap(),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&shown).unwrap()["verified"],
            true,
            "{shown}"
        );
        assert!(!shown.contains("ColumnImpoundOverflow"), "{shown}");
    }
}

#[test]
fn published_candidate_reextraction_is_bounded_and_partial() {
    let files: Vec<(String, &[u8])> = (0..65)
        .map(|i| (format!("file_{i}.rs"), b"pub struct OldName;".as_slice()))
        .collect();
    let borrowed: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(path, bytes)| (path.as_str(), *bytes))
        .collect();
    let (root, cache, store) = search::tests::fixture(&borrowed);
    for (path, _) in &files {
        std::fs::write(root.path().join(path), b"pub struct NewName;").unwrap();
    }
    let result = search::search(
        &store,
        &Query::parse("sym:OldName").unwrap(),
        false,
        cache.path(),
    )
    .unwrap();
    assert_eq!(result.hits.len(), 1);
    assert!(result.hits[0].differs);
    assert!(result.truncated);
    assert_eq!(
        result.coverage["current_source_recheck"]["unsearched_paths"],
        1
    );
}

#[test]
fn refreshed_candidates_do_not_claim_current_rerank_scores() {
    struct Scores;
    impl search::RerankScorer for Scores {
        fn score(
            &self,
            _: &std::path::Path,
            _: &str,
            documents: &[String],
        ) -> anyhow::Result<Vec<f32>> {
            Ok(vec![1.0; documents.len()])
        }
    }
    let (root, cache, store) = search::tests::fixture(&[("notes.md", b"marker before")]);
    std::fs::write(root.path().join("notes.md"), b"marker current").unwrap();
    let result = search::search_prepared(
        &store,
        &Query::parse("marker").unwrap(),
        cache.path(),
        None,
        Some(&Scores),
        &mut search::telemetry::RetrievalTrace::disabled(),
    )
    .unwrap();
    assert!(!result.hits.is_empty());
    assert_eq!(result.coverage["rerank_status"], "unavailable");
    assert!(result.coverage.get("rerank_window").is_none());
    assert!(
        result.hits[0]
            .snippet
            .as_ref()
            .unwrap()
            .text
            .contains("current")
    );
}

#[test]
fn missing_candidates_disappear_and_unreadable_source_is_partial() {
    let (root, cache, store) = search::tests::fixture(&[
        ("gone.rs", b"pub struct OldName;"),
        ("binary.rs", b"pub struct OldName;"),
    ]);
    std::fs::remove_file(root.path().join("gone.rs")).unwrap();
    std::fs::write(root.path().join("binary.rs"), b"\0binary").unwrap();
    let result = search::search(
        &store,
        &Query::parse("sym:OldName").unwrap(),
        false,
        cache.path(),
    )
    .unwrap();
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].path, "binary.rs");
    assert!(result.hits[0].differs);
    assert!(result.truncated);
    assert_eq!(
        result.coverage["current_source_recheck"]["unsearched_paths"],
        1
    );
}
