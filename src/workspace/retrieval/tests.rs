use super::{member_coverage::*, timeout_without_member_answer};
use serde_json::json;

mod cache_progress_tests;

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

#[test]
fn member_timeout_diagnostic_includes_phase_member_root_and_fallback() {
    let coverage = [json!({
        "member": "lunatic",
        "root": "%2Fworktrees%2Flunatic",
        "state": "timed_out",
        "reason": "timed_out: current-source recheck deadline expired",
    })];
    let error =
        timeout_without_member_answer(0, &coverage, Some("current_source_recheck")).unwrap();
    let message = format!("{error:#}");
    assert!(message.starts_with("timed_out: query deadline expired before any member answered;"));
    let (_, details) = message.split_once("details=").unwrap();
    let details: serde_json::Value = serde_json::from_str(details).unwrap();
    assert_eq!(details["member"], "lunatic");
    assert_eq!(details["root"], "%2Fworktrees%2Flunatic");
    assert_eq!(details["phase"], "current_source_recheck");
    assert_eq!(details["fallback"], "show path:FILE");
    assert_eq!(details["absence"], "not_proven");
    assert!(
        details["reason"]
            .as_str()
            .unwrap()
            .contains("current-source recheck")
    );
}

#[test]
fn member_timeout_diagnostic_requires_an_unanswered_timed_out_member() {
    let timed_out = [json!({"member":"lunatic", "state":"timed_out"})];
    assert!(timeout_without_member_answer(1, &timed_out, None).is_none());
    let error = timeout_without_member_answer(0, &timed_out, None).unwrap();
    assert!(format!("{error:#}").contains(r#""phase":"member_processing""#));
    assert!(
        timeout_without_member_answer(
            0,
            &[json!({
                "member":"lunatic",
                "state":"unavailable"
            })],
            None
        )
        .is_none()
    );
}

#[test]
fn expired_workspace_search_identifies_the_unanswered_home() {
    use crate::{
        cli,
        query_deadline::QueryDeadline,
        workspace::{
            WorkspaceConfig, cache_path, result_cache::WorkspaceResults, test_fixture::Fixture,
        },
    };
    let fixture = Fixture::new();
    let arguments = [
        "--workspace",
        fixture.config.to_str().unwrap(),
        "--root",
        fixture.root.path().join("engine").to_str().unwrap(),
        "--cache",
        fixture.cache.path().to_str().unwrap(),
        "--no-daemon",
        "--diagnostics",
        "off",
        "search",
        "sym:SharedThing",
    ]
    .map(str::to_owned);
    let options = cli::parse(&arguments).unwrap();
    let config = WorkspaceConfig::load(&fixture.config).unwrap();
    let cache = cache_path(&config, Some(fixture.cache.path())).unwrap();
    let setup_deadline = QueryDeadline::start();
    WorkspaceResults::create(&cache, &setup_deadline).unwrap();
    let results = WorkspaceResults::open(&cache, &setup_deadline).unwrap();

    let request = super::WorkspaceQuery {
        config: &config,
        cache: &cache,
        results: &results,
        options: &options,
        context: &cli::request_context(&options).unwrap(),
        deadline: QueryDeadline::after(std::time::Duration::ZERO),
        policy: super::HomeIndexPolicy::IndexInline,
    };
    let error =
        super::search_with_policy(&request, &mut crate::semantic::SemanticSession::default())
            .expect_err("an expired home query cannot prove absence");

    let message = format!("{error:#}");
    assert!(message.starts_with("timed_out:"), "{message}");
    let details: serde_json::Value =
        serde_json::from_str(message.split_once("details=").unwrap().1).unwrap();
    assert_eq!(details["member"], "engine");
    assert_eq!(details["phase"], "member_processing");
    assert_eq!(details["absence"], "not_proven");
    assert_eq!(details["fallback"], "show path:FILE");
}

#[test]
fn no_daemon_rerank_reports_every_member_unavailable_and_keeps_fused_order() {
    let fixture = crate::workspace::test_fixture::Fixture::new();
    let words = ["search", "SharedThing marker", "ws:all"];
    let plain = fixture.json("engine", &words);
    let reranked = fixture.json("engine", &[&["--rerank"][..], &words].concat());
    let files = |page: &serde_json::Value| {
        let hits = page["hits"].as_array().unwrap();
        hits.iter()
            .map(|hit| hit["file"].clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(files(&reranked), files(&plain));
    let rows = reranked["coverage"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    for row in rows {
        assert_eq!(row["issues"]["rerank_status"], "unavailable", "{row}");
        let reason = row["issues"]["rerank_reason"].as_str().unwrap();
        assert!(reason.contains("--no-daemon"), "{reason}");
    }
    let rows = plain["coverage"].as_array().unwrap();
    assert!(
        rows.iter()
            .all(|row| row["issues"].get("rerank_status").is_none())
    );
}
