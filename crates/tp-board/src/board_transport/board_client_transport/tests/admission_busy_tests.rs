use super::*;
use tp_core::coded_error::{CodedError, ErrorCode};

fn capability_status(database: &Path) -> String {
    serde_json::json!({
        "status": "ok",
        "board_api": BOARD_API,
        "board_db": database,
    })
    .to_string()
}

fn busy_gateway(database: &Path, error: anyhow::Error) -> FakeGateway {
    FakeGateway {
        replies: VecDeque::from([Ok(Some(capability_status(database))), Err(error)]),
        ..Default::default()
    }
}

fn relayed_busy(detail: &str) -> anyhow::Error {
    CodedError::from_relayed(format!("daemon: daemon_busy: {detail}"))
        .expect("daemon busy refusal is a typed relayed error")
        .into()
}

fn invoke_owner_write(
    words: &[&str],
    gateway: &mut FakeGateway,
    database: &Path,
) -> Result<String> {
    let args: Vec<_> = words.iter().map(|word| (*word).to_owned()).collect();
    let options = crate::board_test_support::parse_board_arguments(&args)?;
    let options = crate::board_test_support::round_trip(&crate::prepare_client(&options)?);
    let forwarded = crate::board_test_support::board_argv(&options);
    let command = board_grammar::parse(&options, None)?;
    let context = RequestContext::new(Some("owner-session".into()), Some("human".into()));
    BoardClientTransport::default().run_prepared(
        &forwarded,
        &options,
        &command,
        &context,
        gateway,
        &mut || Ok(BoardConfig::for_database(database)),
        None,
        &database.parent().unwrap().join("spool"),
        &QueryDeadline::start(),
    )
}

fn assert_typed_busy_is_preserved(error: &anyhow::Error) {
    assert!(
        error.chain().any(|cause| {
            cause
                .downcast_ref::<CodedError>()
                .is_some_and(|coded| coded.code() == ErrorCode::DaemonBusy)
        }),
        "typed daemon_busy error was lost: {error:#}"
    );
}

#[test]
fn owner_accept_and_grant_report_router_queue_no_dispatch_and_manual_recovery() {
    for words in [
        &["board", "accept", "E25725"][..],
        &[
            "board",
            "grant",
            "P23",
            "decide",
            "--to",
            "codex/recovery-test",
        ][..],
    ] {
        let directory = scratch();
        let database = directory.path().join("board.sqlite3");
        let detail = "pool=router-board queue_depth=32 queue_capacity=32 cause=queue_full retry_after_ms=250 hint=retry_with_bounded_backoff";
        let mut gateway = busy_gateway(&database, relayed_busy(detail));

        let error = invoke_owner_write(words, &mut gateway, &database).unwrap_err();
        let message = error.to_string();
        assert_typed_busy_is_preserved(&error);
        assert!(
            message.contains("board_write_outcome=not_dispatched"),
            "{message}"
        );
        assert!(
            message.contains("bounded retry honors at least 250 ms"),
            "{message}"
        );
        assert!(message.contains("--no-daemon"), "{message}");
        assert!(
            message.contains("same command/body and actor/session"),
            "{message}"
        );
        assert!(
            message.contains("router-published Board database"),
            "{message}"
        );
        assert!(message.contains("SQLite WAL/SHM"), "{message}");
        assert!(message.contains("filesystem sandbox"), "{message}");
        assert!(
            message.contains("no direct fallback occurs automatically"),
            "{message}"
        );
        assert!(
            !message.contains("board_write_outcome=unknown"),
            "{message}"
        );
        assert!(!message.contains("request_key="), "{message}");
        assert!(!message.contains("board receipt"), "{message}");
        assert_eq!(
            gateway.requests.len(),
            2,
            "one probe and one rejected write"
        );
        assert!(
            !database.exists(),
            "a rejected write must not use local storage"
        );
    }
}

#[test]
fn incomplete_or_other_busy_details_do_not_claim_a_safe_direct_retry() {
    let refused_details = [
        "pool=router-board queue_depth=31 queue_capacity=32 cause=queue_full retry_after_ms=250 hint=retry_with_bounded_backoff",
        "pool=router-board queue_depth=32 queue_capacity=32 cause=pool_closed retry_after_ms=250 hint=retry_with_bounded_backoff",
        "pool=router-query queue_depth=32 queue_capacity=32 cause=queue_full retry_after_ms=250 hint=retry_with_bounded_backoff",
        "pool=router-board queue_depth=32 queue_capacity=32 cause=queue_full hint=retry_with_bounded_backoff",
        "pool=router-board queue_depth=32 queue_capacity=32 cause=queue_full retry_after_ms=250 hint=retry_with_bounded_backoff queue_depth=31",
        "pool=router-board queue_depth=32 queue_capacity=32 cause=queue_full retry_after_ms=250",
        "project write timed out",
    ];

    for detail in refused_details {
        let directory = scratch();
        let database = directory.path().join("board.sqlite3");
        let mut gateway = busy_gateway(&database, relayed_busy(detail));

        let error = invoke_owner_write(&["board", "accept", "E25725"], &mut gateway, &database)
            .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("daemon_busy"), "{message}");
        assert!(
            !message.contains("board_write_outcome=not_dispatched"),
            "{message}"
        );
        assert!(!message.contains("--no-daemon"), "{message}");
        assert_eq!(gateway.requests.len(), 2);
        assert!(!database.exists());
    }

    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let prose = anyhow::anyhow!(
        "daemon_busy: pool=router-board queue_depth=32 queue_capacity=32 cause=queue_full retry_after_ms=250 hint=retry_with_bounded_backoff"
    );
    let mut gateway = busy_gateway(&database, prose);
    let error =
        invoke_owner_write(&["board", "accept", "E25725"], &mut gateway, &database).unwrap_err();
    let message = error.to_string();
    assert!(
        !message.contains("board_write_outcome=not_dispatched"),
        "{message}"
    );
    assert!(!message.contains("--no-daemon"), "{message}");
}

#[test]
fn ambiguous_write_timeout_keeps_receipt_lookup_without_direct_retry_advice() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let timeout = tp_core::daemon_transport::DaemonReplyTimeout::new(
        Duration::from_secs(30),
        Duration::from_secs(30),
    );
    let mut gateway = busy_gateway(&database, timeout.into());

    let error = invoke(
        &["board", "new", "Ambiguous owner write"],
        &mut gateway,
        &mut BoardClientTransport::default(),
        &database,
        None,
    )
    .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("board_write_outcome=unknown"), "{message}");
    assert!(message.contains("request_key="), "{message}");
    assert!(message.contains("board receipt"), "{message}");
    assert!(message.contains("do not automatically replay"), "{message}");
    assert!(
        !message.contains("board_write_outcome=not_dispatched"),
        "{message}"
    );
    assert!(!message.contains("--no-daemon"), "{message}");
    assert_eq!(gateway.requests.len(), 2);
    assert!(!database.exists());
}
