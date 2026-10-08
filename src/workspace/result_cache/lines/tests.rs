use super::*;
use serde_json::json;

#[test]
fn member_summary_keeps_partial_and_truncated_per_member() {
    let coverage = [
        json!({"member":"lunatic","state":"searched","partial":true,"truncated":false,
               "issues":{"semantic_status":"partial","rerank_status":"ready"}}),
        json!({"member":"tg","state":"searched","partial":false,"truncated":true,
               "issues":{"semantic_status":"unavailable","rerank_status":"unavailable"}}),
        json!({"member":"warm","state":"warming"}),
    ];
    assert_eq!(
        member_coverage_summary(&coverage),
        "lunatic partial; tg complete truncated; warm warming; semantic unavailable; rerank unavailable"
    );
    let ready = [json!({"member":"a","state":"searched","partial":false,
                        "issues":{"semantic_status":"ready"}})];
    assert_eq!(member_coverage_summary(&ready), "a complete");
}

#[test]
fn member_summary_counts_unsearched_files_and_omits_unconfigured_lanes() {
    let coverage = [
        json!({"member":"a","state":"searched","partial":true,"unsearched":3,
               "issues":{"semantic_status":"unavailable",
                         "semantic_reason":"semantic_unavailable: set model_dir in inference.toml",
                         "rerank_status":"unavailable",
                         "rerank_reason":"semantic_worker: model directory is not configured"}}),
        json!({"member":"b","state":"searched","partial":false,"unsearched":0,
               "issues":{"excluded_files":40,"parse_failures":2}}),
    ];
    assert_eq!(
        member_coverage_summary(&coverage),
        "a partial (3 unsearched); b complete"
    );
}

#[test]
fn member_summary_labels_substituted_worktree_root() {
    let coverage = [
        json!({"member":"lunatic","worktree":"feature-x","root":"/tmp/x","state":"searched","partial":false}),
        json!({"member":"tg","state":"warming"}),
    ];
    assert_eq!(
        member_coverage_summary(&coverage),
        "lunatic@feature-x complete; tg warming"
    );
}

fn fallback(differs: Value) -> Value {
    json!({"member":"lunatic","worktree":"lunatic-w3-L1","state":"parent_fallback",
           "home_state":"warming","served_from":"lunatic index","differs":differs,
           "partial":false})
}

#[test]
fn member_summary_names_parent_fallback_and_what_differs() {
    assert_eq!(
        member_coverage_summary(&[fallback(json!(3))]),
        "lunatic@lunatic-w3-L1 warming → served from lunatic index (3 files differ)"
    );
    assert_eq!(
        member_coverage_summary(&[fallback(json!(1))]),
        "lunatic@lunatic-w3-L1 warming → served from lunatic index (1 file differs)"
    );
    assert_eq!(
        member_coverage_summary(&[fallback(json!(0))]),
        "lunatic@lunatic-w3-L1 warming → served from lunatic index (no files differ)"
    );
    assert_eq!(
        member_coverage_summary(&[fallback(Value::Null)]),
        "lunatic@lunatic-w3-L1 warming → served from lunatic index (differences unknown)"
    );
}

#[test]
fn member_summary_keeps_warming_and_unavailable_reasons() {
    let warming = [json!({"member":"lunatic","worktree":"L1","state":"warming",
                          "reason":"no parent index"})];
    assert_eq!(
        member_coverage_summary(&warming),
        "lunatic@L1 warming (no parent index)"
    );
    let unavailable = [
        json!({"member":"lunatic","worktree":"L1","state":"unavailable",
                              "reason":"workspace member lunatic@L1 checkout was replaced"}),
    ];
    assert_eq!(
        member_coverage_summary(&unavailable),
        "lunatic@L1 unavailable (workspace member lunatic@L1 checkout was replaced)"
    );
}

#[test]
fn member_summary_names_unsearched_kinds() {
    let coverage = [
        json!({"member":"a","state":"searched","partial":true,"unsearched":20,
                           "unsearched_kinds":["fact_limit"]}),
    ];
    assert_eq!(
        member_coverage_summary(&coverage),
        "a partial (20 unsearched: fact_limit)"
    );
}

#[test]
fn differing_hit_lines_end_with_differs() {
    let hit = |handle: &str, differs: bool| OwnedEntry {
        owner: 0,
        member_rank: 1,
        entry: ResultEntry::LiveSource(crate::results::Hit {
            handle: handle.into(),
            path: "a.rs".into(),
            revision: None,
            start: 0,
            end: 1,
            start_line: 1,
            end_line: 1,
            name: "a".into(),
            kind: "function".into(),
            container: None,
            provenance: None,
            resolution: None,
            candidates: Vec::new(),
            target: None,
            repeats: None,
            snippet: None,
            differs: false,
        }),
        worktree_differs: differs,
    };
    let text = "s:1\tm/a.rs:1-1\ta\n  1: fn a\ns:2\tm/a.rs:1-1\ncoverage: m complete\n";
    assert_eq!(
        mark_worktree_differs(text.into(), &[hit("s:1", true), hit("s:2", false)]),
        "s:1\tm/a.rs:1-1\ta\tdiffers\n  1: fn a\ns:2\tm/a.rs:1-1\ncoverage: m complete\n"
    );
}

#[test]
fn member_summary_names_timed_out_members() {
    let coverage = [
        json!({"member":"a","state":"searched","partial":false}),
        json!({"member":"b","state":"timed_out"}),
    ];
    assert_eq!(
        member_coverage_summary(&coverage),
        "a complete; b timed out"
    );
}
