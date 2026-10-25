use super::*;
use std::{cell::RefCell, collections::BTreeSet, path::Path, time::Duration};
use tempfile::TempDir;
use tp_index::search::search_prepared;

/// Records every scored document; scores each by its `rank N` marker so an
/// unchanged file can outrank the file ahead of it in fused order.
struct RecordingScorer {
    documents: RefCell<Vec<String>>,
    failure: Option<&'static str>,
}

impl RerankScorer for RecordingScorer {
    fn score(&self, _: &Path, _: &str, documents: &[String], _: Duration) -> Result<Vec<f32>> {
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
        documents: RefCell::new(Vec::new()),
        failure,
    }
}

/// A parent index over `a.md`, `b.md`, and `notes.md`, read from a worktree
/// whose `notes.md` changed, so the worktree recheck replaces its hit.
struct ParentAnswer {
    _dirs: [TempDir; 4],
    store: Store,
}

impl ParentAnswer {
    fn new() -> Self {
        let dirs = [(); 4].map(|_| tempfile::tempdir().unwrap());
        let [parent, cache, worktree, results] = &dirs;
        for (dir, notes) in [
            (parent, "marker stale indexed text"),
            (worktree, "marker current text"),
        ] {
            std::fs::write(dir.path().join("a.md"), "marker rank low").unwrap();
            std::fs::write(dir.path().join("b.md"), "marker rank high").unwrap();
            std::fs::write(dir.path().join("notes.md"), notes).unwrap();
        }
        Store::open(parent.path(), cache.path())
            .unwrap()
            .index()
            .unwrap();
        let store = Store::open_read(parent.path(), cache.path(), QueryDeadline::start())
            .unwrap()
            .reading_from(worktree.path(), results.path())
            .unwrap();
        Self { _dirs: dirs, store }
    }

    /// Searches the parent index without reranking, then completes the
    /// answer from worktree bytes with `scorer` as its rerank.
    fn search(&self, scorer: Option<&RecordingScorer>) -> ResultSet {
        let query = Query::parse("marker").unwrap();
        let deadline = QueryDeadline::start();
        let mut trace = RetrievalTrace::disabled();
        let mut found = search_prepared(
            &self.store,
            &query,
            self.store.index_cache(),
            None,
            None,
            &mut trace,
            &deadline,
        )
        .unwrap();
        let mut fallback = ParentFallback {
            divergence: WorktreeDivergence {
                paths: BTreeSet::from(["notes.md".to_owned()]),
                complete: true,
            },
            served_from: "parent index".into(),
        };
        let words = ["search".to_owned(), "marker".to_owned()];
        let rerank = scorer.map(|scorer| ParentRerank {
            scorer,
            trace: &mut trace,
        });
        let changed = fallback
            .complete_answer(
                &self.store,
                ("search", &words, &query, &InvocationDirectory::root()),
                &mut found,
                &mut WorktreeHashes::default(),
                rerank,
                &deadline,
            )
            .unwrap();
        assert_eq!(changed, HashSet::from(["notes.md".to_owned()]));
        found
    }
}

fn paths(found: &ResultSet) -> Vec<&str> {
    found.hits.iter().map(|hit| hit.path.as_str()).collect()
}

#[test]
fn parent_answer_replaced_hit_stays_unscored() {
    let answer = ParentAnswer::new();
    let scorer = recording(None);
    let found = answer.search(Some(&scorer));
    let documents = scorer.documents.borrow();
    assert_eq!(documents.len(), 2, "{documents:?}");
    assert!(
        documents
            .iter()
            .all(|document| !document.contains("stale") && !document.contains("current text")),
        "{documents:?}"
    );
    assert_eq!(found.coverage["rerank_status"], "ready");
    assert_eq!(found.coverage["rerank_window"], 2);
    assert!(found.coverage.get("rerank_reason").is_none());
    let replaced = found
        .hits
        .iter()
        .find(|hit| hit.path == "notes.md")
        .expect("replaced hit kept");
    assert!(
        replaced
            .provenance
            .as_deref()
            .is_some_and(|provenance| provenance.starts_with("worktree_current:")),
        "{replaced:?}"
    );
}

#[test]
fn parent_answer_reranks_unchanged_hits_after_recheck() {
    let answer = ParentAnswer::new();
    let fused = answer.search(None);
    let fused = paths(&fused);
    let low = fused.iter().position(|path| *path == "a.md").unwrap();
    let high = fused.iter().position(|path| *path == "b.md").unwrap();
    assert!(
        low < high,
        "fixture expects a.md ahead in fused order: {fused:?}"
    );

    let found = answer.search(Some(&recording(None)));
    // Scored hits lead in score order; the unscored replacement follows.
    assert_eq!(paths(&found), ["b.md", "a.md", "notes.md"]);
}

#[test]
fn parent_answer_keeps_rechecked_order_when_reranker_is_unavailable() {
    let answer = ParentAnswer::new();
    let fused = answer.search(None);
    assert!(fused.coverage.get("rerank_status").is_none());
    let found = answer.search(Some(&recording(Some("rerank_worker_unreachable"))));
    assert_eq!(paths(&found), paths(&fused));
    assert_eq!(found.coverage["rerank_status"], "unavailable");
    assert!(
        found.coverage["rerank_reason"]
            .as_str()
            .unwrap()
            .contains("rerank_worker_unreachable"),
        "{}",
        found.coverage
    );
    assert!(found.coverage.get("rerank_window").is_none());
}
