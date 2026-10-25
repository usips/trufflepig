use super::*;
use crate::workspace::result_cache::{MemberSnapshot, OwnedEntry, WorkspaceSet};
use serde_json::json;
use std::{cell::RefCell, path::Path, time::Duration};
use tp_core::query_deadline::QueryDeadline;
use tp_index::{
    results::Hit,
    search::{self, Query, RERANK_WINDOW, telemetry::Lane},
    store::Store,
};

const MEMBERS: [&str; 2] = ["alpha", "beta"];
const FILES_PER_MEMBER: usize = 20;

/// Records each request; scores beta documents above alpha ones, and lower
/// file numbers higher within a member, unless every score is `flat`.
#[derive(Default)]
struct RecordingScorer {
    requests: RefCell<Vec<(usize, Duration)>>,
    failure: Option<&'static str>,
    flat: bool,
}

impl RerankScorer for RecordingScorer {
    fn score(
        &self,
        _cache_identity: &Path,
        _query: &str,
        documents: &[String],
        budget: Duration,
    ) -> anyhow::Result<Vec<f32>> {
        self.requests.borrow_mut().push((documents.len(), budget));
        if let Some(failure) = self.failure {
            // The worker client raises its relayed failures as coded errors.
            let coded = tp_core::coded_error::CodedError::from_relayed(failure.to_owned());
            anyhow::bail!(coded.expect("fake failures carry a registered code"));
        }
        Ok(documents
            .iter()
            .map(|document| {
                let number: f32 = document.trim().rsplit(' ').next().unwrap().parse().unwrap();
                let member = if document.contains("beta") {
                    100.0
                } else {
                    50.0
                };
                if self.flat { 1.0 } else { member - number }
            })
            .collect())
    }
}

struct Workspace {
    _dirs: Vec<tempfile::TempDir>,
    fused: FusedMembers,
    member_hits: Vec<Vec<Hit>>,
}

/// Two indexed members, each with `FILES_PER_MEMBER` hits for `marker`,
/// interleaved round-robin as member fusion does.
fn workspace() -> Workspace {
    let query = Query::parse("marker").unwrap();
    let mut dirs = Vec::new();
    let mut stores = Vec::new();
    let mut member_hits = Vec::new();
    for member in MEMBERS {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        for number in 0..FILES_PER_MEMBER {
            let body = format!("marker {member} {number}\n");
            std::fs::write(root.path().join(format!("{member}{number:02}.md")), body).unwrap();
        }
        let mut store = Store::open(root.path(), cache.path()).unwrap();
        store.index().unwrap();
        let found = search::search_prepared(
            &store,
            &query,
            cache.path(),
            None,
            None,
            &mut RetrievalTrace::disabled(),
            &QueryDeadline::start(),
        )
        .unwrap();
        assert_eq!(found.hits.len(), FILES_PER_MEMBER);
        member_hits.push(found.hits);
        stores.push(Some(store));
        dirs.extend([root, cache]);
    }
    let mut hits = Vec::new();
    for rank in 0..FILES_PER_MEMBER {
        for (owner, member) in member_hits.iter().enumerate() {
            hits.push(OwnedEntry {
                owner,
                member_rank: rank + 1,
                entry: ResultEntry::LiveSource(member[rank].clone()),
                worktree_differs: false,
            });
        }
    }
    let owners = MEMBERS.map(|name| MemberSnapshot {
        name: name.to_owned(),
        root: String::new(),
        cache: String::new(),
        device: 0,
        inode: 0,
        index_identity: String::new(),
        generation: 1,
        coverage: json!({}),
        worktree: None,
        parent_index: None,
    });
    let set = WorkspaceSet {
        workspace: "space".to_owned(),
        home: None,
        owners: owners.to_vec(),
        // A warming member's row precedes the owners' rows.
        coverage: vec![
            json!({"member": "cold", "state": "warming"}),
            json!({"member": "alpha", "state": "searched", "issues": {}}),
            json!({"member": "beta", "state": "searched", "issues": {}}),
        ],
        hits,
        truncated: false,
        scope: None,
    };
    let fused = FusedMembers {
        set,
        owner_rows: vec![1, 2],
        stores,
    };
    Workspace {
        _dirs: dirs,
        fused,
        member_hits,
    }
}

fn rerank(
    workspace: &mut Workspace,
    scorer: &RecordingScorer,
    deadline: &QueryDeadline,
) -> RerankOutcome {
    let cache = tempfile::tempdir().unwrap();
    let mut trace = RetrievalTrace::default();
    let query = Query::parse("marker").unwrap();
    let rerank = MergedRerank {
        query: &query,
        cache: cache.path(),
        scorer,
        deadline,
    };
    let outcome = rerank_merged_window(&mut workspace.fused, &rerank, &mut trace);
    let lane = trace
        .lanes
        .iter()
        .find(|lane| lane.lane == Lane::Rerank)
        .unwrap();
    if matches!(outcome, RerankOutcome::Ready { .. }) {
        assert_eq!(lane.candidate_count, RERANK_WINDOW);
    }
    assert!(workspace.fused.stores.is_empty(), "stores are released");
    outcome
}

fn paths(set: &WorkspaceSet) -> Vec<String> {
    set.hits
        .iter()
        .map(|entry| match &entry.entry {
            ResultEntry::LiveSource(hit) => hit.path.clone(),
            _ => unreachable!(),
        })
        .collect()
}

/// Every entry still names the member and member rank that produced its hit.
fn assert_attribution(workspace: &Workspace) {
    for entry in &workspace.fused.set.hits {
        let ResultEntry::LiveSource(hit) = &entry.entry else {
            unreachable!()
        };
        let original = &workspace.member_hits[entry.owner][entry.member_rank - 1];
        assert_eq!(hit.path, original.path);
        assert!(hit.path.starts_with(MEMBERS[entry.owner]));
    }
}

#[test]
fn one_request_reorders_the_merged_window_across_members() {
    let mut workspace = workspace();
    let fused_order = paths(&workspace.fused.set);
    let scorer = RecordingScorer::default();
    let outcome = rerank(&mut workspace, &scorer, &QueryDeadline::start());

    assert_eq!(
        outcome,
        RerankOutcome::Ready {
            scored: RERANK_WINDOW
        }
    );
    let requests = scorer.requests.borrow();
    assert_eq!(requests.len(), 1, "one worker request for the workspace");
    assert_eq!(requests[0].0, RERANK_WINDOW);
    let reranked = paths(&workspace.fused.set);
    let window = RERANK_WINDOW / 2;
    let beta: Vec<_> = (0..window).map(|n| format!("beta{n:02}.md")).collect();
    let alpha: Vec<_> = (0..window).map(|n| format!("alpha{n:02}.md")).collect();
    assert_eq!(reranked[..window], beta[..]);
    assert_eq!(reranked[window..RERANK_WINDOW], alpha[..]);
    assert_eq!(reranked[RERANK_WINDOW..], fused_order[RERANK_WINDOW..]);
    assert_attribution(&workspace);

    let set = &workspace.fused.set;
    for (owner, row) in [(0, 1), (1, 2)] {
        assert_eq!(set.owners[owner].coverage["rerank_status"], "ready");
        assert_eq!(set.owners[owner].coverage["rerank_window"], window);
        assert_eq!(set.coverage[row]["issues"]["rerank_status"], "ready");
    }
    assert!(set.coverage[0].get("issues").is_none());
}

#[test]
fn equal_scores_keep_the_fused_order() {
    let mut workspace = workspace();
    let fused_order = paths(&workspace.fused.set);
    let scorer = RecordingScorer {
        flat: true,
        ..RecordingScorer::default()
    };
    rerank(&mut workspace, &scorer, &QueryDeadline::start());
    assert_eq!(paths(&workspace.fused.set), fused_order);
    assert_attribution(&workspace);
}

#[test]
fn the_request_budget_is_the_query_deadline_remainder() {
    let mut workspace = workspace();
    let scorer = RecordingScorer::default();
    let remaining = Duration::from_millis(400);
    rerank(&mut workspace, &scorer, &QueryDeadline::after(remaining));
    let budget = scorer.requests.borrow()[0].1;
    assert!(budget <= remaining, "{budget:?}");
    assert!(budget > Duration::ZERO);

    let mut workspace = self::workspace();
    let scorer = RecordingScorer::default();
    rerank(&mut workspace, &scorer, &QueryDeadline::start());
    assert_eq!(
        scorer.requests.borrow()[0].1,
        tp_semantic::worker::RERANK_DEADLINE
    );
}

#[test]
fn an_expired_deadline_keeps_the_fused_order_without_a_request() {
    let mut workspace = workspace();
    let fused_order = paths(&workspace.fused.set);
    let scorer = RecordingScorer::default();
    let outcome = rerank(
        &mut workspace,
        &scorer,
        &QueryDeadline::after(Duration::ZERO),
    );
    assert!(scorer.requests.borrow().is_empty());
    let RerankOutcome::Unavailable(reason) = outcome else {
        panic!("{outcome:?}")
    };
    assert!(reason.starts_with("semantic_timeout:"), "{reason}");
    assert_eq!(paths(&workspace.fused.set), fused_order);
    let set = &workspace.fused.set;
    assert_eq!(set.coverage[1]["issues"]["rerank_status"], "unavailable");
    assert_eq!(set.coverage[2]["issues"]["rerank_reason"], reason);
    assert!(set.owners[0].coverage.get("rerank_window").is_none());
}

#[test]
fn an_unavailable_reranker_keeps_the_fused_order_for_every_member() {
    let mut workspace = workspace();
    let fused_order = paths(&workspace.fused.set);
    let scorer = RecordingScorer {
        failure: Some("semantic_loading: inference worker is starting"),
        ..RecordingScorer::default()
    };
    let outcome = rerank(&mut workspace, &scorer, &QueryDeadline::start());
    assert_eq!(scorer.requests.borrow().len(), 1);
    assert!(matches!(outcome, RerankOutcome::Unavailable(_)));
    assert_eq!(paths(&workspace.fused.set), fused_order);
    assert_attribution(&workspace);
    for (owner, row) in [(0, 1), (1, 2)] {
        let set = &workspace.fused.set;
        assert_eq!(set.owners[owner].coverage["rerank_status"], "unavailable");
        assert_eq!(
            set.coverage[row]["issues"]["rerank_reason"],
            "semantic_loading: inference worker is starting"
        );
    }
}

#[test]
fn a_hit_replaced_from_current_source_stays_unscored() {
    let mut workspace = workspace();
    // The first merged hit now names bytes the index never stored.
    let ResultEntry::LiveSource(hit) = &mut workspace.fused.set.hits[0].entry else {
        unreachable!()
    };
    hit.revision = Some("0".repeat(64));
    let replaced = hit.path.clone();
    let scorer = RecordingScorer::default();
    let outcome = rerank(&mut workspace, &scorer, &QueryDeadline::start());
    assert_eq!(
        outcome,
        RerankOutcome::Ready {
            scored: RERANK_WINDOW - 1
        }
    );
    assert_eq!(scorer.requests.borrow()[0].0, RERANK_WINDOW - 1);
    assert_eq!(paths(&workspace.fused.set)[RERANK_WINDOW - 1], replaced);
}
