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
    let body = "# Edge acceptance\n\n- Keep markdown intact\n\u{001b}[31mtext\n";
    let forwarded = board_grammar::normalize_args(&options, Some(body)).unwrap();
    assert!(
        !crate::board::board_test_support::board_argv(&forwarded)
            .iter()
            .any(|arg| arg.contains("client-only.md"))
    );
    let router_options = crate::board::board_test_support::round_trip(&forwarded);
    assert_eq!(router_options.words, ["board", "new"]);
    assert_eq!(router_options.board.body, None);
    fixture
        .host
        .run(
            &router_options,
            &context("codex", "actual-session"),
            QueryDeadline::start(),
        )
        .unwrap();

    let human = fixture.actor("human", "grant-session");
    let grant_options = fixture.options(&[
        "board",
        "grant",
        "P1",
        "decide",
        "--to",
        "codex/actual-session",
    ]);
    let forwarded = board_grammar::normalize_args(&grant_options, None).unwrap();
    let grant_options = crate::board::board_test_support::round_trip(&forwarded);
    let grant_context = RequestContext::with_origin(
        Some("grant-session".into()),
        Some("human".into()),
        Some(human.origin()),
    );
    let grant_response = fixture
        .host
        .run(&grant_options, &grant_context, QueryDeadline::start())
        .unwrap();
    let grant_reply: Value = serde_json::from_str(&grant_response).unwrap();
    let grant_entry = data(&grant_reply)["entry"].as_str().unwrap();
    let grant_entry = grant_entry
        .strip_prefix('E')
        .unwrap()
        .parse::<i64>()
        .unwrap();
    let origin: (String, String) = rusqlite::Connection::open(&fixture.config.db_path)
        .unwrap()
        .query_row(
            "SELECT origin_harness, origin_session FROM entries WHERE id=?1",
            [grant_entry],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(origin, ("human".into(), "grant-session".into()));

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
    let audit = concat!(
        r#"[{"verb":"show","args":["path:src/lib.rs:1-10"],"exit_code":1,"#,
        r#""error_prefix":"stale_source","truncated":true,"coverage":"partial"}]"#
    );
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
    let reports = data(&listed)["feedback"].as_array().unwrap();
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
    let body = "\u{001f}".repeat(crate::board::board_vocabulary::ENTRY_TEXT_LIMIT);
    let escaped_args = crate::board::board_test_support::board_argv(
        &board_grammar::normalize_args(&options, Some(&body)).unwrap(),
    );
    let request_context = context("codex", "encoded-session");
    let escaped_size =
        tp_core::daemon_transport::request_encoded_size(&escaped_args, &request_context).unwrap();
    let expected_request = serde_json::json!({
        "command": "Arguments",
        "arguments": {
            "context": &request_context,
            "args": &escaped_args,
        },
    });
    assert_eq!(
        escaped_size,
        serde_json::to_vec(&expected_request).unwrap().len()
    );
    let plain_body = "x".repeat(crate::board::board_vocabulary::ENTRY_TEXT_LIMIT);
    let plain_args = crate::board::board_test_support::board_argv(
        &board_grammar::normalize_args(&options, Some(&plain_body)).unwrap(),
    );
    let plain_size =
        tp_core::daemon_transport::request_encoded_size(&plain_args, &request_context).unwrap();
    assert_eq!(body.len(), crate::board::board_vocabulary::ENTRY_TEXT_LIMIT);
    assert!(
        escaped_size > plain_size,
        "nested JSON escaping was not counted"
    );
    assert!(
        escaped_size <= tp_core::daemon_transport::MAX_DAEMON_REQUEST_BYTES,
        "maximum entry text unexpectedly exceeds the request limit: {escaped_size}"
    );
    assert!(
        !fixture.config.db_path.exists(),
        "normalization opened storage"
    );
}
