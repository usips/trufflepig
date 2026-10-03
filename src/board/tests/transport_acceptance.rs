use super::*;

#[test]
fn normalized_markdown_and_body_preserve_client_context_and_ssot() {
    let fixture = EdgeFixture::new();
    fixture.run(
        &["board", "hello", "gpt-6.1-sol", "xhigh"],
        "codex",
        "actual-session",
    );
    let mut options = fixture.options(&["board", "new", "--", "- Edge", "plan"]);
    options.board.body = Some(PathBuf::from("client-only.md"));
    options.board.steward = Some("codex".into());
    options.client = Some("human".into());
    options.session = Some("forged-session".into());
    let body = "# Edge acceptance\n\n- Keep markdown intact\n\u{001b}[31mtext\n";
    let forwarded = board_grammar::normalize_args(&[], &options, Some(body)).unwrap();
    assert!(!forwarded.iter().any(|arg| arg.contains("client-only.md")));
    let router_options = cli::parse(&forwarded).unwrap();
    assert_eq!(router_options.words, ["board", "new"]);
    assert_eq!(router_options.board.body, None);
    assert!(router_options.client.is_none());
    assert!(router_options.session.is_none());
    fixture
        .host
        .run(
            &router_options,
            &context("codex", "actual-session"),
            QueryDeadline::start(),
        )
        .unwrap();

    let shown = fixture.run(&["board", "show", "P1"], "codex", "actual-session");
    let view = data(&shown);
    assert_eq!(view["plan"]["title"], "- Edge plan");
    assert_eq!(view["revision"]["body"], body);
    assert_eq!(view["revision"]["actor"]["harness"], "codex");
    assert_eq!(view["revision"]["actor"]["session"], "actual-session");
    assert!(
        !fixture
            .config
            .db_path
            .parent()
            .unwrap()
            .join("index.sqlite3")
            .exists()
    );

    let mut proposal = fixture.options(&["board", "propose", "P1@1", "--", "- Change", "scope"]);
    proposal.board.body = Some(PathBuf::from("also-client-only.md"));
    let change = fixture
        .run_options(
            proposal,
            "codex",
            "actual-session",
            Some("# Changed\n- New body\n"),
        )
        .unwrap();
    let entry = data(&change)["entry"].as_str().unwrap();
    fixture.run(&["board", "accept", entry], "codex", "actual-session");
    let old = fixture.run(&["board", "show", "P1@1"], "codex", "actual-session");
    assert_eq!(data(&old)["body"], body);
    let head = fixture.run(&["board", "show", "P1"], "codex", "actual-session");
    assert_eq!(data(&head)["revision"]["body"], "# Changed\n- New body\n");
}

#[test]
fn feedback_forwarding_keeps_audit_body_and_model_claims_with_local_repo_context() {
    let fixture = EdgeFixture::new();
    fixture.seed_repo();
    std::fs::create_dir(fixture.root.join("nested")).unwrap();
    fixture.run(
        &["board", "hello", "gpt-6.1-sol", "xhigh"],
        "codex",
        "feedback-session",
    );
    let audit = r#"[{"verb":"show","args":["path:src/lib.rs:1-10"],"exit_code":1,"error_prefix":"stale_source","truncated":true,"coverage":"partial"}]"#;
    let mut options = fixture.options_at(
        &fixture.root.join("nested"),
        &["feedback", "blocked", "--", "- Read", "failed"],
    );
    options.board.body = Some(PathBuf::from("private-client-body.md"));
    options.board.agent_model = Some("gpt-6.1-sol".into());
    options.board.agent_effort = Some("xhigh".into());
    options.board.recent_calls = Some(audit.into());
    let body = "Tried show.\nGot stale_source.\nUsed a targeted read.\n";
    fixture
        .run_options(options, "codex", "feedback-session", Some(body))
        .unwrap();
    let listed = fixture.run(&["feedback", "ls", "--open"], "codex", "feedback-session");
    let reports = data(&listed).as_array().unwrap();
    assert_eq!(reports.len(), 1);
    let report = &reports[0];
    assert!(report["entry"]["body"].as_str().unwrap().contains(body));
    assert!(
        report["entry"]["body"]
            .as_str()
            .unwrap()
            .starts_with("- Read failed")
    );
    assert_eq!(report["entry"]["model"], "gpt-6.1-sol");
    assert_eq!(report["entry"]["effort"], "xhigh");
    assert_eq!(report["entry"]["actor"]["session"], "feedback-session");
    assert_eq!(report["metadata"]["cwd"], "nested");
    assert!(report["metadata"]["repo_key"].as_str().is_some());
    assert_eq!(
        report["metadata"]["recent_calls"][0]["error_prefix"],
        "stale_source"
    );
    assert_eq!(report["metadata"]["recent_calls"][0]["truncated"], true);
    assert_eq!(report["metadata"]["recent_calls"][0]["coverage"], "partial");
}

#[test]
fn encoded_frame_measurement_includes_nested_body_escaping_and_audit_metadata() {
    let fixture = EdgeFixture::new();
    let mut options = fixture.options(&["board", "new", "Escaped body"]);
    options.board.body = Some(PathBuf::from("body.md"));
    let body = "\u{001f}".repeat(16_384);
    let args = board_grammar::normalize_args(&[], &options, Some(&body)).unwrap();
    let size =
        crate::daemon::request_encoded_size(&args, &context("codex", "encoded-session")).unwrap();
    assert!(body.len() < 32_768);
    assert!(
        size > crate::daemon::MAX_DAEMON_REQUEST_BYTES,
        "encoded request unexpectedly fits: {size}"
    );
    assert!(
        !fixture.config.db_path.exists(),
        "normalization opened storage"
    );
}
