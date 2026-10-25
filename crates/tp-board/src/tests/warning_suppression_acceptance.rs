use super::*;

fn split_footer(fixture: &EdgeFixture) -> String {
    git(
        &fixture.root,
        &[
            "commit",
            "--allow-empty",
            "-m",
            "Split footer\n\nPlan: P1\n\nPlan-Task: P1.1\n\nCo-authored-by: Model claim <noreply@openai.com>",
        ],
    );
    git(&fixture.root, &["rev-parse", "HEAD"])
}

fn warning_fixture() -> EdgeFixture {
    let fixture = EdgeFixture::new();
    fixture.seed_repo();
    fixture.run(&["board", "new", "Warning repairs"], "human", "owner");
    fixture.run(&["board", "task", "P1", "Repair lane"], "human", "owner");
    fixture
}

#[test]
fn ingest_cache_hit_drops_warning_after_manual_link() {
    let fixture = warning_fixture();
    let oid = split_footer(&fixture);
    let first = fixture.run(&["board", "ingest"], "codex", "reviewer");
    let warning = format!("board_scan: {oid}: misplaced_trailers");
    assert!(
        first["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item == &warning)
    );
    fixture.run(&["board", "link", &oid, "P1.1"], "human", "owner");
    let second = fixture.run(&["board", "ingest"], "codex", "reviewer");
    assert_eq!(data(&second)["inserted"], 0);
    assert!(
        second["warnings"]
            .as_array()
            .is_none_or(|warnings| warnings.iter().all(|item| item != &warning)),
        "a manual link must suppress the cached oid warning: {second}"
    );
}

#[test]
fn review_of_other_plan_omits_linked_oid_warnings() {
    let fixture = warning_fixture();
    fixture.run(&["board", "new", "Other plan"], "human", "owner");
    let oid = split_footer(&fixture);
    fixture.run(&["board", "ingest"], "codex", "reviewer");
    fixture.run(&["board", "link", &oid, "P1.1"], "human", "owner");
    let review = fixture.run(&["board", "review", "P2@1", "codex"], "codex", "reviewer");
    let packet = data(&review);
    assert!(packet["linked"].as_array().unwrap().is_empty());
    assert!(
        packet["scan_errors"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| {
                item.as_str() != Some(&format!("board_scan: {oid}: misplaced_trailers"))
            }),
        "links in any plan suppress warnings while reviewing another plan: {packet}"
    );
}
