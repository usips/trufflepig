use super::*;
use serde_json::json;

#[test]
fn rerank_unavailable_marks_partial_coverage_and_surfaces_reason() {
    let coverage = json!({
        "rerank_status": "unavailable",
        "rerank_reason": "rerank_worker_unreachable",
    });
    assert!(partial_coverage(&coverage));
    let issues = coverage_issues(&coverage);
    assert_eq!(issues["rerank_status"], "unavailable");
    assert_eq!(issues["rerank_reason"], "rerank_worker_unreachable");
}

#[test]
fn rerank_ready_is_not_partial_but_still_reported() {
    let coverage = json!({
        "rerank_status": "ready",
        "rerank_window": 12,
    });
    assert!(!partial_coverage(&coverage));
    let issues = coverage_issues(&coverage);
    assert_eq!(issues["rerank_status"], "ready");
    assert!(issues.get("rerank_reason").is_none());
}

#[test]
fn rerank_skipped_carries_no_partial_or_issue_state() {
    let coverage = json!({"rerank_status": "skipped"});
    assert!(!partial_coverage(&coverage));
    assert_eq!(coverage_issues(&coverage)["rerank_status"], "skipped");
}

#[test]
fn missing_rerank_keys_are_absent_from_issues() {
    let coverage = json!({});
    assert!(!partial_coverage(&coverage));
    assert!(coverage_issues(&coverage).get("rerank_status").is_none());
    assert!(coverage_issues(&coverage).get("rerank_reason").is_none());
}
