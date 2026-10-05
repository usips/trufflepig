use super::*;

#[test]
fn manual_link_repairs_an_untrailered_commit_through_host_resolution() {
    let fixture = EdgeFixture::new();
    fixture.seed_repo();
    fixture.run(
        &["board", "new", "Manual link", "--steward", "codex"],
        "human",
        "owner",
    );
    fixture.run(&["board", "task", "P1", "Repair lane"], "human", "owner");
    std::fs::write(fixture.root.join("repair.txt"), "untrailered work\n").unwrap();
    git(&fixture.root, &["add", "repair.txt"]);
    git(
        &fixture.root,
        &[
            "commit",
            "-m",
            "Repair by hand\n\nCo-authored-by: Model claim <noreply@openai.com>",
        ],
    );
    let oid = git(&fixture.root, &["rev-parse", "HEAD"]);
    fixture.run(&["board", "ingest"], "codex", "reviewer");
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM commit_plans"),
        0,
        "an untrailered commit is never scan-linked"
    );
    let reply = fixture.run(&["board", "link", &oid, "P1.1"], "human", "owner");
    let change = data(&reply);
    assert_eq!(change["task"], "P1.1");
    assert_eq!(change["plan"], "P1");
    assert_eq!(change["deduplicated"], false);
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM commit_plans"), 1);
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM commit_plans WHERE source='manual'"),
        1
    );
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM commit_tasks WHERE source='manual'"),
        1
    );
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM commits WHERE subject='Repair by hand'"),
        1
    );
    let again = fixture.run(&["board", "link", &oid, "P1.1"], "human", "owner");
    let repeated = data(&again);
    assert_eq!(repeated["deduplicated"], true);
    assert_eq!(repeated["entry"], change["entry"]);
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM commit_plans"), 1);
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM entries WHERE kind='commit'"),
        1,
        "a repeated manual link creates no duplicate entry"
    );
}

#[test]
fn manual_link_rejects_an_unknown_oid_before_writing() {
    let fixture = EdgeFixture::new();
    fixture.seed_repo();
    fixture.run(&["board", "new", "Manual link"], "human", "owner");
    fixture.run(&["board", "task", "P1", "Repair lane"], "human", "owner");
    let error = fixture
        .run_options(
            fixture.options(&["board", "link", &"f".repeat(40), "P1.1"]),
            "human",
            "owner",
            None,
        )
        .unwrap_err();
    assert!(
        error.to_string().starts_with("invalid_reference:"),
        "{error:#}"
    );
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM commit_plans"), 0);
}
