use super::{recheck, recheck_with_current_search};
use crate::{
    daemon::deadline::QueryDeadline,
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

/// Records every scored document; scores each by its `rank N` marker so an
/// unchanged file can outrank the files ahead of it in fused order.
struct RecordingScorer {
    documents: std::cell::RefCell<Vec<String>>,
    failure: Option<&'static str>,
}

impl search::RerankScorer for RecordingScorer {
    fn score(
        &self,
        _: &std::path::Path,
        _: &str,
        documents: &[String],
        _: std::time::Duration,
    ) -> anyhow::Result<Vec<f32>> {
        self.documents.borrow_mut().extend_from_slice(documents);
        if let Some(failure) = self.failure {
            anyhow::bail!("{failure}");
        }
        Ok(documents
            .iter()
            .map(|document| document.contains("rank high") as u8 as f32)
            .collect())
    }
}

fn recording(failure: Option<&'static str>) -> RecordingScorer {
    RecordingScorer {
        documents: std::cell::RefCell::new(Vec::new()),
        failure,
    }
}

/// `a.md` and `b.md` stay as indexed; `notes.md` changes after publishing, so
/// the current-source recheck replaces its hit.
fn replaced_fixture() -> (tempfile::TempDir, tempfile::TempDir, crate::store::Store) {
    let (root, cache, store) = search::tests::fixture(&[
        ("a.md", b"marker rank low"),
        ("b.md", b"marker rank high"),
        ("notes.md", b"marker stale indexed text"),
    ]);
    std::fs::write(root.path().join("notes.md"), b"marker current text").unwrap();
    (root, cache, store)
}

fn rerank_search(
    store: &crate::store::Store,
    cache: &std::path::Path,
    scorer: Option<&RecordingScorer>,
) -> results::ResultSet {
    search::search_prepared(
        store,
        &Query::parse("marker").unwrap(),
        cache,
        None,
        scorer.map(|scorer| scorer as &dyn search::RerankScorer),
        &mut search::telemetry::RetrievalTrace::disabled(),
        &QueryDeadline::start(),
    )
    .unwrap()
}

#[test]
fn replaced_candidate_stays_unscored_after_current_source_recheck() {
    let (_root, cache, store) = replaced_fixture();
    let scorer = recording(None);
    let result = rerank_search(&store, cache.path(), Some(&scorer));
    let documents = scorer.documents.borrow();
    assert!(
        documents.iter().all(|document| !document.contains("stale")
            && !document.contains("current text")),
        "{documents:?}"
    );
    assert_eq!(documents.len(), 2, "{documents:?}");
    assert_eq!(result.coverage["rerank_status"], "ready");
    assert_eq!(result.coverage["rerank_window"], 2);
    assert!(result.coverage.get("rerank_reason").is_none());
    let replaced = result
        .hits
        .iter()
        .find(|hit| hit.path == "notes.md")
        .expect("replaced hit kept");
    assert!(replaced.snippet.as_ref().unwrap().text.contains("current"));
}

#[test]
fn unchanged_candidates_are_reranked_after_current_source_recheck() {
    let (_root, cache, store) = replaced_fixture();
    let fused = rerank_search(&store, cache.path(), None);
    let fused: Vec<_> = fused.hits.iter().map(|hit| hit.path.as_str()).collect();
    let low = fused.iter().position(|path| *path == "a.md").unwrap();
    let high = fused.iter().position(|path| *path == "b.md").unwrap();
    assert!(low < high, "fixture expects a.md ahead in fused order: {fused:?}");

    let scorer = recording(None);
    let result = rerank_search(&store, cache.path(), Some(&scorer));
    let paths: Vec<_> = result.hits.iter().map(|hit| hit.path.as_str()).collect();
    // Scored hits lead their tier in score order; the unscored replacement follows.
    assert_eq!(paths, ["b.md", "a.md", "notes.md"]);
}

#[test]
fn unavailable_reranker_keeps_the_rechecked_fused_order() {
    let (_root, cache, store) = replaced_fixture();
    let fused = rerank_search(&store, cache.path(), None);
    let scorer = recording(Some("rerank_worker_unreachable"));
    let result = rerank_search(&store, cache.path(), Some(&scorer));
    let paths = |set: &results::ResultSet| -> Vec<String> {
        set.hits.iter().map(|hit| hit.path.clone()).collect()
    };
    assert_eq!(paths(&result), paths(&fused));
    assert_eq!(result.coverage["rerank_status"], "unavailable");
    assert!(
        result.coverage["rerank_reason"]
            .as_str()
            .unwrap()
            .contains("rerank_worker_unreachable")
    );
    assert!(result.coverage.get("rerank_window").is_none());
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

#[test]
fn current_search_timeout_propagates_but_ordinary_failure_keeps_unverified_candidate() {
    let old = b"pub struct OldName;";
    let current = b"pub struct NewName;";
    let (root, cache, store) = search::tests::fixture(&[("lib.rs", old)]);
    let query = Query::parse("OldName file:lib.rs").unwrap();
    let initial = search::search(&store, &query, false, cache.path()).unwrap();
    assert!(!initial.hits.is_empty());
    let mut timeout_hits = initial.hits.clone();
    std::fs::write(root.path().join("lib.rs"), current).unwrap();
    let mut timeout_coverage = serde_json::json!({});
    let mut timeout_truncated = false;
    let timeout = recheck_with_current_search(
        &store,
        &query,
        &mut timeout_hits,
        &mut timeout_coverage,
        &mut timeout_truncated,
        &QueryDeadline::start(),
        |_, _, path, bytes, _, _| {
            assert_eq!(path, "lib.rs");
            assert_eq!(bytes, current);
            Err(
                anyhow::anyhow!(coded!(TimedOut, "current file search interrupted"))
                    .context("search_current_file"),
            )
        },
    )
    .unwrap_err();
    assert!(
        crate::coded_error::ErrorCode::TimedOut.is_code_of(&timeout),
        "{timeout:#}"
    );
    assert!(timeout.to_string().starts_with("timed_out:"));

    let mut ordinary_hits = initial.hits;
    let mut coverage = serde_json::json!({});
    let mut truncated = false;
    recheck_with_current_search(
        &store,
        &query,
        &mut ordinary_hits,
        &mut coverage,
        &mut truncated,
        &QueryDeadline::start(),
        |_, _, path, bytes, _, _| {
            assert_eq!(path, "lib.rs");
            assert_eq!(bytes, current);
            Err(anyhow::anyhow!("ordinary current extraction failure"))
        },
    )
    .unwrap();
    assert!(truncated);
    assert!(!ordinary_hits.is_empty());
    assert!(ordinary_hits.iter().all(|hit| hit.differs));
    assert_eq!(coverage["current_source_unsearched"], 1);
    assert_eq!(coverage["current_source_recheck"]["status"], "incomplete");
}

#[test]
fn current_search_timeout_under_another_coded_layer_still_propagates() {
    use crate::coded_error::CodedContext;
    let (root, cache, store) = search::tests::fixture(&[("lib.rs", b"pub struct OldName;")]);
    let query = Query::parse("OldName file:lib.rs").unwrap();
    let mut hits = search::search(&store, &query, false, cache.path())
        .unwrap()
        .hits;
    assert!(!hits.is_empty());
    std::fs::write(root.path().join("lib.rs"), b"pub struct NewName;").unwrap();
    let error = recheck_with_current_search(
        &store,
        &query,
        &mut hits,
        &mut serde_json::json!({}),
        &mut false,
        &QueryDeadline::start(),
        |_, _, _, _, _, _| {
            Err(anyhow::anyhow!(coded!(TimedOut, "current file search interrupted"))
                .with_coded_context(|| coded!(SourceUnavailable, "current snapshot")))
        },
    )
    .unwrap_err();
    assert!(
        crate::coded_error::ErrorCode::TimedOut.is_code_of(&error),
        "{error:#}"
    );
}

#[test]
fn expired_query_deadline_aborts_candidate_recheck() {
    let (root, cache, store) = search::tests::fixture(&[("lib.rs", b"pub struct OldName;")]);
    let query = Query::parse("OldName file:lib.rs").unwrap();
    let initial = search::search(&store, &query, false, cache.path()).unwrap();
    assert!(!initial.hits.is_empty());
    std::fs::write(root.path().join("lib.rs"), b"pub struct NewName;").unwrap();
    let mut hits = initial.hits;
    let mut coverage = serde_json::json!({});
    let mut truncated = false;

    let error = recheck(
        &store,
        &query,
        &mut hits,
        &mut coverage,
        &mut truncated,
        &QueryDeadline::after(std::time::Duration::ZERO),
    )
    .unwrap_err();

    assert!(
        crate::coded_error::ErrorCode::TimedOut.is_code_of(&error),
        "{error:#}"
    );
    assert!(error.to_string().starts_with("timed_out:"));
    assert!(!hits.is_empty());
}
