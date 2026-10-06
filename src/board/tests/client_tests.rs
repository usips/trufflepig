use crate::board::{
    board_grammar::{self, BoardCommand},
    board_protocol::{self, BoardOp},
    domain_answer, domain_error, prepare_client,
};

#[test]
fn captured_body_survives_file_changes_between_retries() {
    let directory = crate::board::board_test_support::scratch("board-transport-");
    let body_path = directory.path().join("plan.md");
    std::fs::write(&body_path, "- captured plan\n").unwrap();
    let args = vec![
        "--body".into(),
        body_path.to_string_lossy().into_owned(),
        "board".into(),
        "new".into(),
        "Trial".into(),
    ];
    let options = crate::cli::parse(&args).unwrap();
    let prepared = prepare_client(&args, &options).unwrap();
    std::fs::write(&body_path, "changed before replay").unwrap();
    let prepared_options = crate::cli::parse(&prepared).unwrap();
    let replay = prepare_client(&prepared, &prepared_options).unwrap();
    let replay_options = crate::cli::parse(&replay).unwrap();
    let BoardCommand::Op(BoardOp::New { body, .. }) =
        board_grammar::parse(&replay_options, None).unwrap()
    else {
        panic!("expected a new plan");
    };
    assert_eq!(body.as_str(), "- captured plan\n");
    assert!(replay_options.board.body.is_none());
}

#[test]
fn body_preflight_precedes_file_reads_and_byte_limits_precede_utf8() {
    let args: Vec<String> = [
        "--body",
        "missing-body-file",
        "board",
        "propose",
        "P0@1",
        "summary",
    ]
    .map(str::to_owned)
    .into();
    let options = crate::cli::parse(&args).unwrap();
    let error = prepare_client(&args, &options).unwrap_err();
    assert!(
        error.to_string().starts_with("invalid_reference:"),
        "{error:#}"
    );
    let directory = crate::board::board_test_support::scratch("board-runtime-");
    let path = directory.path().join("oversized-feedback");
    std::fs::write(&path, vec![0xff; 4097]).unwrap();
    let args = vec![
        "--body".into(),
        path.to_string_lossy().into_owned(),
        "feedback".into(),
        "blocked".into(),
        "summary".into(),
    ];
    let options = crate::cli::parse(&args).unwrap();
    let error = prepare_client(&args, &options).unwrap_err();
    assert!(
        error.to_string().contains("body exceeds 4096 bytes"),
        "{error:#}"
    );
}

#[test]
fn typed_transient_cause_wins_over_domain_looking_context() {
    let error = anyhow::Error::new(board_protocol::BoardError::new(
        board_protocol::BoardErrorCode::BoardUnavailable,
        "temporarily offline",
    ))
    .context("invalid_body: context is diagnostic prose");
    assert!(!domain_error(&error));
    assert!(!domain_error(
        &anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
            .context("budget_too_small: diagnostic context")
    ));
}

#[test]
fn feedback_domain_answers_are_not_queued() {
    for code in [
        "board_remote_unsupported",
        "board_api_mismatch",
        "invalid_reference",
        "usage",
        "stale_revision",
        "claim_conflict",
        "budget_too_small",
    ] {
        for prefix in ["", "daemon: ", "daemon: daemon: "] {
            assert!(domain_answer(&format!("{prefix}{code}: failure")));
        }
    }
    for message in [
        "read frame failed: invalid_body: incidental prose",
        "write failed: invalid_options: diagnostic mention",
        "unknown_code: stale_revision: detail",
        "read frame failed: invalid_body: incidental prose",
        "write failed: invalid_options: diagnostic mention",
        "unknown_code: stale_revision: detail",
        "board_unavailable: permission denied",
        "daemon: read frame: broken pipe",
        "database is locked",
    ] {
        assert!(!domain_answer(message));
    }
}

#[test]
fn oversized_write_reports_size_code_instead_of_invalid_state() {
    let sqlite = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_TOOBIG),
        Some("String or BLOB exceeds size limit".into()),
    );
    let error = anyhow::Error::new(sqlite).context("store entry");
    assert_eq!(
        board_protocol::BoardErrorCode::from_error(&error),
        Some(board_protocol::BoardErrorCode::InvalidBody)
    );
    assert!(domain_error(&error));
    let rendered = board_protocol::BoardError::from(error).to_string();
    assert!(
        rendered.starts_with("invalid_body:"),
        "CLI shows the size error, got: {rendered}"
    );
}

#[test]
fn newer_schema_refusal_advises_upgrade() {
    let directory = crate::board::board_test_support::scratch("board-schema-");
    let config = crate::board::BoardConfig::for_database(directory.path().join("board.sqlite3"));
    let args: Vec<String> = ["board", "inbox"].map(str::to_owned).into();
    let options = crate::cli::parse(&args).unwrap();
    let command = board_grammar::parse(&options, None).unwrap();
    let context = crate::diagnostics::RequestContext::new(None, None);
    let error = anyhow::anyhow!("board_unavailable: schema version 99 is newer than supported 7");
    let refusal = crate::board::unavailable_or_queue(
        &command,
        &options,
        &context,
        &config,
        directory.path(),
        error,
    )
    .unwrap_err();
    assert_eq!(
        refusal.to_string(),
        "board_unavailable: database schema 99 is newer than this trufflepig (7); \
         upgrade trufflepig"
    );
}
