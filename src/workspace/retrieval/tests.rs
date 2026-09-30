use super::member_coverage::*;
use serde_json::json;

#[test]
fn unavailable_lanes_surface_reasons_without_marking_lexical_coverage_partial() {
    let coverage = json!({
        "rerank_status": "unavailable",
        "rerank_reason": "rerank_worker_unreachable",
        "semantic_status": "unavailable",
        "excluded_files": 20,
        "parse_failures": 18,
    });
    assert!(!partial_coverage(&coverage));
    let issues = coverage_issues(&coverage);
    assert_eq!(issues["rerank_status"], "unavailable");
    assert_eq!(issues["rerank_reason"], "rerank_worker_unreachable");
    assert_eq!(issues["parse_failures"], 18);
}

#[test]
fn unsearched_files_mark_partial_coverage_with_a_count() {
    let coverage = json!({"walk_failures": 1, "live_read_failures": 2});
    assert!(partial_coverage(&coverage));
    assert_eq!(unsearched_files(&coverage), 3);
    assert_eq!(unsearched_kinds(&coverage), ["walk_error", "read_error"]);
    let limited = json!({"truncated_files": 20, "unsearched_paths": ["big.rs"]});
    assert_eq!(unsearched_kinds(&limited), ["fact_limit"]);
    assert_eq!(
        coverage_issues(&limited)["unsearched_paths"],
        json!(["big.rs"])
    );
}

#[test]
fn excluded_live_files_are_issues_not_partial() {
    let coverage = json!({"live_excluded_files": 4, "live_read_failures": 0});
    assert!(!partial_coverage(&coverage));
    assert_eq!(coverage_issues(&coverage)["live_excluded_files"], 4);
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
