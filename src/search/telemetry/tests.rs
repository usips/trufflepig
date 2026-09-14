use super::*;
use crate::{
    search::{self, Query},
    semantic::SemanticSession,
    store::Store,
};

fn observed(query: &str) -> (crate::results::ResultSet, RetrievalTrace) {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("alpha.rs"),
        b"fn alpha() {}\nfn caller() { alpha(); }\n",
    )
    .unwrap();
    let mut store = Store::open(root.path(), cache.path()).unwrap();
    store.index().unwrap();
    let mut trace = RetrievalTrace::default();
    let result = search::search_with_session(
        &store,
        &Query::parse(query).unwrap(),
        false,
        cache.path(),
        &mut SemanticSession::default(),
        &mut trace,
    )
    .unwrap();
    (result, trace)
}

#[test]
fn retrieval_observations_preserve_lane_rank_and_preimage() {
    let (result, trace) = observed("alpha");
    assert_eq!(
        trace
            .lanes
            .iter()
            .map(|event| event.lane)
            .collect::<Vec<_>>(),
        [Lane::ExactIdentifier, Lane::Lexical, Lane::File]
    );
    assert_eq!(trace.generation, Some(result.generation));
    let exact = &trace.lanes[0].candidates[0];
    assert_eq!(exact.original_rank, 1);
    assert_eq!(exact.path, result.hits[0].path);
    assert_eq!(exact.revision, result.hits[0].revision);
    assert_eq!(
        (exact.start, exact.end),
        (result.hits[0].start, result.hits[0].end)
    );
    assert!(
        trace
            .lanes
            .iter()
            .all(|event| event.outcome == LaneOutcome::Complete)
    );
    let json = serde_json::to_string(&trace).unwrap();
    assert!(!json.contains("fn alpha"));
    assert!(!json.contains("caller"));
}

#[test]
fn retrieval_observations_do_not_invent_skipped_lanes() {
    let (_, exact) = observed("sym:alpha");
    assert_eq!(exact.lanes.len(), 1);
    assert_eq!(exact.lanes[0].lane, Lane::ExactIdentifier);
    let (_, regex) = observed("re:alpha");
    assert_eq!(regex.lanes.len(), 1);
    assert_eq!(regex.lanes[0].lane, Lane::LiveRegex);
    assert_eq!(regex.coverage.unwrap().live_checked_files, Some(1));
    let (_, filtered) = observed("alpha kind:function");
    assert!(!filtered.lanes.iter().any(|event| event.lane == Lane::File));
}

#[test]
fn retrieval_observations_report_bounded_identity_loss() {
    let (result, _) = observed("sym:alpha");
    let hits = vec![result.hits[0].clone(); MAX_LANE_IDENTITIES + 3];
    let mut trace = RetrievalTrace::default();
    trace.record(
        Lane::ExactIdentifier,
        Instant::now(),
        &hits,
        LaneOutcome::Complete,
        false,
    );
    assert_eq!(trace.lanes[0].candidate_count, MAX_LANE_IDENTITIES + 3);
    assert_eq!(trace.lanes[0].candidate_records_omitted, 3);
    assert_eq!(
        trace.lanes[0].candidates.last().unwrap().original_rank,
        MAX_LANE_IDENTITIES
    );
    let mut disabled = RetrievalTrace::disabled();
    disabled.record(
        Lane::ExactIdentifier,
        Instant::now(),
        &hits,
        LaneOutcome::Complete,
        false,
    );
    assert!(disabled.lanes.is_empty());
}
#[test]
fn retrieval_observations_bound_serialized_metadata() {
    let (mut result, _) = observed("sym:alpha");
    result.hits[0].path = "a\\\"".repeat(2048);
    let hits = vec![result.hits[0].clone(); 128];
    let mut trace = RetrievalTrace::default();
    for lane in [
        Lane::ExactIdentifier,
        Lane::Lexical,
        Lane::File,
        Lane::Semantic,
    ] {
        trace.record(lane, Instant::now(), &hits, LaneOutcome::Complete, false);
    }
    assert!(serde_json::to_vec(&trace).unwrap().len() < 32 * 1024);
    assert!(
        trace
            .lanes
            .iter()
            .all(|event| event.candidate_records_omitted > 0)
    );
}

#[test]
fn retrieval_observations_retain_failed_lane_without_error_text() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut store = Store::open(root.path(), cache.path()).unwrap();
    store.index().unwrap();
    let mut trace = RetrievalTrace::default();
    let result = search::search_with_session(
        &store,
        &Query::parse("re:[private-pattern").unwrap(),
        false,
        cache.path(),
        &mut SemanticSession::default(),
        &mut trace,
    );
    assert!(result.is_err());
    assert_eq!(trace.lanes.len(), 1);
    assert_eq!(trace.lanes[0].lane, Lane::LiveRegex);
    assert_eq!(trace.lanes[0].outcome, LaneOutcome::Failed);
    assert!(
        !serde_json::to_string(&trace)
            .unwrap()
            .contains("private-pattern")
    );
}

#[cfg(not(feature = "semantic"))]
#[test]
fn retrieval_observations_identify_semantic_preparation_failure() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut store = Store::open(root.path(), cache.path()).unwrap();
    store.index().unwrap();
    let mut trace = RetrievalTrace::default();
    let result = search::search_with_session(
        &store,
        &Query::parse("private-query").unwrap(),
        true,
        cache.path(),
        &mut SemanticSession::default(),
        &mut trace,
    );
    assert!(result.is_err());
    assert_eq!(trace.lanes.len(), 1);
    assert_eq!(trace.lanes[0].lane, Lane::Semantic);
    assert_eq!(trace.lanes[0].outcome, LaneOutcome::Failed);
    assert!(trace.generation.is_none());
    assert!(trace.query_preparation_us.is_some());
    assert!(
        !serde_json::to_string(&trace)
            .unwrap()
            .contains("private-query")
    );
}
