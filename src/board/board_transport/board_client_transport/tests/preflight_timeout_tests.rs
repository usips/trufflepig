use super::*;

fn daemon_timeout() -> anyhow::Error {
    tp_core::daemon_transport::DaemonReplyTimeout::new(
        Duration::from_secs(30),
        Duration::from_secs(30),
    )
    .into()
}

fn invoke_counted(
    words: &[&str],
    gateway: &mut FakeGateway,
    database: &Path,
    runtime: Option<&Path>,
    loads: &mut usize,
) -> Result<String> {
    let args: Vec<_> = words.iter().map(|word| (*word).to_owned()).collect();
    invoke_args_counted(&args, gateway, database, runtime, loads)
}

fn invoke_args_counted(
    args: &[String],
    gateway: &mut FakeGateway,
    database: &Path,
    runtime: Option<&Path>,
    loads: &mut usize,
) -> Result<String> {
    let options = crate::board::board_test_support::parse_board_arguments(args)?;
    let options =
        crate::board::board_test_support::round_trip(&crate::board::prepare_client(&options)?);
    let forwarded = crate::board::board_test_support::board_argv(&options);
    let command = board_grammar::parse(&options, None)?;
    let spool = database.parent().unwrap().join("spool");
    BoardClientTransport::default().run_prepared(
        &forwarded,
        &options,
        &command,
        &RequestContext::new(None, None),
        gateway,
        &mut || {
            *loads += 1;
            Ok(BoardConfig::for_database(database))
        },
        runtime,
        &spool,
        &QueryDeadline::start(),
    )
}

fn assert_preflight_timeout(error: &anyhow::Error) {
    let message = error.to_string();
    assert!(
        message.starts_with("daemon_reply_timeout: phase=awaiting_reply"),
        "{message}"
    );
    assert!(
        message.contains("board_write_outcome=not_dispatched"),
        "{message}"
    );
    assert!(
        message.contains("board_preflight=system_status"),
        "{message}"
    );
    assert!(message.contains("--no-daemon"), "{message}");
    assert!(
        !message.contains("board_write_outcome=unknown"),
        "{message}"
    );
    assert!(!message.contains("request_key="), "{message}");
    assert!(!message.contains("board receipt"), "{message}");
    assert!(
        tp_core::daemon_transport::DaemonReplyTimeout::from_error(error).is_some(),
        "typed timeout was lost: {error:?}"
    );
}

#[test]
fn initial_status_timeout_for_new_and_hello_is_not_dispatched() {
    for words in [
        &["board", "new", "Timed out plan"][..],
        &["board", "hello", "gpt-6-luna"][..],
    ] {
        let directory = scratch();
        let database = directory.path().join("board.sqlite3");
        let mut gateway = FakeGateway {
            replies: VecDeque::from([Err(daemon_timeout())]),
            ..Default::default()
        };
        let mut loads = 0;

        let error = invoke_counted(words, &mut gateway, &database, None, &mut loads).unwrap_err();
        assert_preflight_timeout(&error);
        assert_eq!(
            gateway.requests,
            [vec![String::from("system"), String::from("status")]]
        );
        assert_eq!(gateway.ensured, 0);
        assert_eq!(loads, 0, "preflight diagnostics must not load Board config");
        assert!(!database.exists());
    }
}

#[test]
fn retry_status_timeout_after_ensure_never_dispatches_a_board_operation() {
    let directory = scratch();
    let runtime = directory.path().join("runtime");
    let database = directory.path().join("board.sqlite3");
    let mut gateway = FakeGateway {
        replies: VecDeque::from([Ok(None), Err(daemon_timeout())]),
        ..Default::default()
    };
    let mut loads = 0;

    let error = invoke_counted(
        &["board", "new", "Retry timed out"],
        &mut gateway,
        &database,
        Some(&runtime),
        &mut loads,
    )
    .unwrap_err();

    assert_preflight_timeout(&error);
    assert_eq!(
        gateway.requests,
        [
            vec![String::from("system"), String::from("status")],
            vec![String::from("system"), String::from("status")]
        ]
    );
    assert_eq!(gateway.ensured, 1);
    assert_eq!(loads, 0, "preflight diagnostics must not load Board config");
    assert!(!database.exists());
}

#[test]
fn read_timeout_and_unrelated_write_error_keep_their_original_classification() {
    let directory = scratch();
    let database = directory.path().join("read.sqlite3");
    let mut gateway = FakeGateway {
        replies: VecDeque::from([Err(daemon_timeout())]),
        ..Default::default()
    };
    let error = invoke(
        &["board", "show"],
        &mut gateway,
        &mut BoardClientTransport::default(),
        &database,
        None,
    )
    .unwrap_err();
    let message = error.to_string();
    assert!(message.starts_with("daemon_reply_timeout:"), "{message}");
    assert!(!message.contains("board_write_outcome"), "{message}");
    assert!(!message.contains("board_preflight="), "{message}");
    assert_eq!(
        gateway.requests,
        [vec![String::from("system"), String::from("status")]]
    );
    assert!(!database.exists());

    let database = directory.path().join("unrelated.sqlite3");
    let mut gateway = FakeGateway {
        replies: VecDeque::from([Err(anyhow::anyhow!("injected non-timeout failure"))]),
        ..Default::default()
    };
    let error = invoke(
        &["board", "new", "Not dispatched"],
        &mut gateway,
        &mut BoardClientTransport::default(),
        &database,
        None,
    )
    .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("injected non-timeout failure"),
        "{message}"
    );
    assert!(
        !message.contains("board_write_outcome=not_dispatched"),
        "{message}"
    );
    assert!(!message.contains("board_preflight="), "{message}");
    assert_eq!(
        gateway.requests,
        [vec![String::from("system"), String::from("status")]]
    );
    assert!(!database.exists());
}

#[test]
fn preflight_feedback_timeout_still_queues_durable_outbox_record() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let args = feedback_args(
        &directory.path(),
        "preflight feedback",
        "preserve this body",
    );
    let mut gateway = FakeGateway {
        replies: VecDeque::from([Err(daemon_timeout())]),
        ..Default::default()
    };
    let mut loads = 0;

    let reply = invoke_args_counted(&args, &mut gateway, &database, None, &mut loads).unwrap();

    let key = assert_queued_reply(&reply);
    assert_queued_feedback(
        &directory.path().join("spool"),
        "preflight feedback",
        "preserve this body",
    );
    assert!(!key.is_empty());
    assert_eq!(
        gateway.requests,
        [vec![String::from("system"), String::from("status")]]
    );
    assert_eq!(gateway.ensured, 0);
    assert_eq!(
        loads, 1,
        "feedback loads config to persist its outbox record"
    );
    assert!(!database.exists());
}
