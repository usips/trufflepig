use super::*;

#[test]
fn unlink_cli_from_an_unrelated_checkout_preserves_registrations() {
    let fixture = EdgeFixture::new();
    fixture.seed_repo();
    fixture.run(
        &["board", "new", "Manual unlink", "--steward", "codex"],
        "human",
        "owner",
    );
    fixture.run(&["board", "task", "P1", "Repair lane"], "human", "owner");
    let oid = git(&fixture.root, &["rev-parse", "HEAD"]);
    fixture.run(&["board", "link", &oid, "P1.1"], "human", "owner");
    let other = fixture.root.parent().unwrap().join("unrelated");
    std::fs::create_dir(&other).unwrap();
    git(&other, &["init", "--initial-branch=main"]);
    std::fs::write(other.join("unrelated.txt"), "unrelated checkout\n").unwrap();
    git(&other, &["add", "unrelated.txt"]);
    git(&other, &["commit", "-m", "Unrelated root"]);
    std::fs::remove_dir_all(fixture.root.join(".git")).unwrap();
    let before = ["repos", "repo_paths", "plan_repos"]
        .map(|table| fixture.scalar(&format!("SELECT COUNT(*) FROM {table}")));
    let reply = fixture
        .run_options(
            fixture.options_at(&other, &["board", "unlink", &oid, "P1.1"]),
            "human",
            "owner",
            None,
        )
        .unwrap();
    let change = data(&reply);
    assert_eq!(change["deduplicated"], false);
    assert_eq!(change["task"], "P1.1");
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM commit_tasks"), 0);
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM commit_plans"), 0);
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM events WHERE kind='unlinked'"),
        1
    );
    let replay = fixture
        .run_options(
            fixture.options_at(&other, &["board", "unlink", &oid, "P1.1"]),
            "codex",
            "second-caller",
            None,
        )
        .unwrap();
    assert_eq!(data(&replay)["deduplicated"], true);
    assert_eq!(data(&replay)["entry"], change["entry"]);
    assert_eq!(data(&replay)["seq"], change["seq"]);
    let after = ["repos", "repo_paths", "plan_repos"]
        .map(|table| fixture.scalar(&format!("SELECT COUNT(*) FROM {table}")));
    assert_eq!(after, before);
    assert!(
        reply
            .get("warnings")
            .is_none_or(|warnings| warnings == &serde_json::json!([]))
    );
}

#[test]
fn unlink_cli_rejects_unknown_links_without_registration() {
    let fixture = EdgeFixture::new();
    fixture.run(&["board", "new", "Manual unlink"], "human", "owner");
    fixture.run(&["board", "task", "P1", "Repair lane"], "human", "owner");
    let before = fixture.scalar("SELECT COUNT(*) FROM events");
    let error = fixture
        .run_options(
            fixture.options(&["board", "unlink", &"f".repeat(40), "P1.1"]),
            "human",
            "owner",
            None,
        )
        .unwrap_err();
    assert!(
        error.to_string().starts_with("invalid_reference:"),
        "{error}"
    );
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM events"), before);
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM repos"), 0);
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM repo_paths"), 0);
}

#[test]
fn unlink_scanned_cli_link_returns_only_after_changed_tips_are_ingested() {
    let fixture = EdgeFixture::new();
    fixture.seed_repo();
    fixture.run(&["board", "new", "Scanned unlink"], "human", "owner");
    fixture.run(&["board", "task", "P1", "Trailer lane"], "human", "owner");
    std::fs::write(fixture.root.join("task.txt"), "task work\n").unwrap();
    git(&fixture.root, &["add", "task.txt"]);
    git(
        &fixture.root,
        &["commit", "-m", "Task work\n\nPlan: P1\nPlan-Task: P1.1"],
    );
    let oid = git(&fixture.root, &["rev-parse", "HEAD"]);
    fixture.run(&["board", "ingest"], "human", "owner");
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM commit_tasks WHERE source='scan'"),
        1
    );
    fixture.run(&["board", "unlink", &oid, "P1.1"], "human", "owner");
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM commit_tasks"), 0);
    fixture.run(&["board", "ingest"], "human", "owner");
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM commit_tasks"),
        0,
        "unchanged tips skip metadata ingestion"
    );
    std::fs::write(fixture.root.join("next.txt"), "next work\n").unwrap();
    git(&fixture.root, &["add", "next.txt"]);
    git(&fixture.root, &["commit", "-m", "Advance branch"]);
    fixture.run(&["board", "ingest"], "human", "owner");
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM commit_tasks WHERE source='scan'"),
        1
    );
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM events WHERE kind='unlinked'"),
        1
    );
}
