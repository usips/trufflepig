use super::*;

fn hand_linked_kimi_commit() -> (EdgeFixture, String, Value) {
    let fixture = EdgeFixture::new();
    fixture.seed_repo();
    fixture.run(&["board", "new", "Hand linker review"], "human", "owner");
    fixture.run(&["board", "task", "P1", "Reviewed lane"], "human", "owner");
    git(
        &fixture.root,
        &[
            "commit",
            "--allow-empty",
            "-m",
            "Untrailered Kimi work\n\nCo-authored-by: Kimi K2 <noreply@moonshot.ai>",
        ],
    );
    let oid = git(&fixture.root, &["rev-parse", "HEAD"]);
    let receipt = fixture.run(&["board", "link", &oid, "P1.1"], "human", "hand-linker");
    (fixture, oid, data(&receipt).clone())
}

#[test]
fn review_with_agent_names_hand_linker() {
    let (fixture, oid, receipt) = hand_linked_kimi_commit();
    let linker = fixture.actor("human", "hand-linker");
    let expected = format!(
        "task P1.1 linked by hand by {} (entry {}, seq {})",
        linker.identity(),
        receipt["entry"].as_str().unwrap(),
        receipt["seq"].as_u64().unwrap(),
    );
    for words in [
        &["board", "review", "P1@1"][..],
        &["board", "review", "P1@1", "kimi"][..],
    ] {
        let mut options = fixture.options(words);
        options.format = "lines".into();
        let review = fixture
            .host
            .run(
                &options,
                &context("codex", "reviewer"),
                QueryDeadline::start(),
            )
            .unwrap();
        assert!(review.contains(&oid), "{review}");
        assert!(review.contains("coauthor: kimi (Kimi K2)"), "{review}");
        assert!(review.contains(&expected), "{review}");
    }
}

#[test]
fn review_json_manual_links_names_hand_linker() {
    let (fixture, oid, receipt) = hand_linked_kimi_commit();
    for words in [
        &["board", "review", "P1@1"][..],
        &["board", "review", "P1@1", "kimi"][..],
    ] {
        let review = fixture.run(words, "codex", "reviewer");
        let packet = data(&review);
        if words.len() == 4 {
            assert_eq!(packet["agent"], "kimi");
            assert!(packet["entries"].as_array().unwrap().is_empty());
        }
        let linked = packet["linked"].as_array().unwrap();
        assert_eq!(linked.len(), 1);
        assert_eq!(linked[0]["oid"], oid);
        let manual_links = linked[0]["manual_links"].as_array().unwrap();
        assert_eq!(manual_links.len(), 1);
        assert_eq!(
            manual_links[0],
            serde_json::json!({
                "repo_key": linked[0]["repo_key"],
                "oid": oid,
                "task": "P1.1",
                "entry": receipt["entry"],
                "seq": receipt["seq"],
                "linked_by": fixture.actor("human", "hand-linker"),
            }),
        );
    }
}

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

#[test]
fn failed_link_leaves_plan_repos_unchanged() {
    let fixture = EdgeFixture::new();
    fixture.seed_repo();
    fixture.run(&["board", "new", "Manual link"], "human", "owner");
    fixture.run(&["board", "task", "P1", "Repair lane"], "human", "owner");
    let other = fixture.root.parent().unwrap().join("other");
    std::fs::create_dir(&other).unwrap();
    git(&other, &["init", "--initial-branch=main"]);
    std::fs::write(other.join("other.txt"), "other repository\n").unwrap();
    git(&other, &["add", "other.txt"]);
    git(&other, &["commit", "-m", "Other root"]);
    let before = fixture.scalar("SELECT COUNT(*) FROM plan_repos");
    let error = fixture
        .run_options(
            fixture.options_at(&other, &["board", "link", &"f".repeat(40), "P1.1"]),
            "human",
            "owner",
            None,
        )
        .unwrap_err();
    assert!(
        error.to_string().starts_with("invalid_reference:"),
        "{error:#}"
    );
    assert_eq!(
        fixture.scalar("SELECT COUNT(*) FROM plan_repos"),
        before,
        "a failed link must not register the caller's repository"
    );
}

#[test]
fn tag_oid_is_refused() {
    let fixture = EdgeFixture::new();
    fixture.seed_repo();
    fixture.run(&["board", "new", "Manual link"], "human", "owner");
    fixture.run(&["board", "task", "P1", "Repair lane"], "human", "owner");
    git(
        &fixture.root,
        &["tag", "-a", "release", "-m", "tagged release"],
    );
    let tag = git(&fixture.root, &["rev-parse", "release"]);
    let error = fixture
        .run_options(
            fixture.options(&["board", "link", &tag, "P1.1"]),
            "human",
            "owner",
            None,
        )
        .unwrap_err();
    let text = format!("{error:#}");
    assert!(text.starts_with("invalid_reference:"), "{text}");
    assert!(text.contains("names a tag; pass the commit id"), "{text}");
    assert_eq!(fixture.scalar("SELECT COUNT(*) FROM commit_plans"), 0);
}
