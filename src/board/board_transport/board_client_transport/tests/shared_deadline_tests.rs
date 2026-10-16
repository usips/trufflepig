use super::*;

fn capability_status(database: &Path) -> String {
    serde_json::json!({
        "status": "ok",
        "board_api": BOARD_API,
        "board_db": database,
    })
    .to_string()
}

#[test]
fn capability_probe_and_dispatch_share_the_callers_remaining_budget() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let mut gateway = FakeGateway {
        replies: VecDeque::from([
            Ok(Some(capability_status(&database))),
            Ok(Some("dispatched".to_owned())),
        ]),
        request_delay: Duration::from_millis(10),
        ..Default::default()
    };
    let reply = invoke_with_deadline(
        &["board", "show"],
        &mut gateway,
        &mut BoardClientTransport::default(),
        &database,
        None,
        QueryDeadline::after(Duration::from_secs(1)),
    )
    .unwrap();

    assert_eq!(reply, "dispatched");
    assert_eq!(gateway.requests.len(), 2);
    assert!(gateway.request_remaining[1] < gateway.request_remaining[0]);
    assert!(!gateway.request_remaining[1].is_zero());
    assert!(observations_share_expiry(&gateway.deadline_expiry_windows));
}

#[test]
fn router_ensure_and_status_retry_keep_the_callers_deadline() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let mut gateway = FakeGateway {
        replies: VecDeque::from([
            Ok(None),
            Ok(Some(capability_status(&database))),
            Ok(Some("dispatched".to_owned())),
        ]),
        request_delay: Duration::from_millis(10),
        ..Default::default()
    };
    let reply = invoke_with_deadline(
        &["board", "show"],
        &mut gateway,
        &mut BoardClientTransport::default(),
        &database,
        None,
        QueryDeadline::after(Duration::from_secs(1)),
    )
    .unwrap();

    assert_eq!(reply, "dispatched");
    assert_eq!(gateway.ensured, 1);
    assert!(gateway.request_remaining[0] > gateway.ensure_remaining[0]);
    assert!(gateway.ensure_remaining[0] > gateway.request_remaining[1]);
    assert!(gateway.request_remaining[1] > gateway.request_remaining[2]);
    assert!(observations_share_expiry(&gateway.deadline_expiry_windows));
}

#[test]
fn expired_capability_probe_does_not_dispatch_the_board_command() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let mut gateway = FakeGateway {
        replies: VecDeque::from([Ok(Some(capability_status(&database)))]),
        request_delay: Duration::from_millis(150),
        ..Default::default()
    };
    let error = invoke_with_deadline(
        &["board", "show"],
        &mut gateway,
        &mut BoardClientTransport::default(),
        &database,
        None,
        QueryDeadline::after(Duration::from_millis(100)),
    )
    .unwrap_err();

    assert!(error.to_string().starts_with("timed_out:"), "{error}");
    assert_eq!(gateway.requests.len(), 1);
    assert!(!gateway.request_remaining[0].is_zero());
    assert_eq!(gateway.ensured, 0);
    assert!(!database.exists());
    assert!(!directory.path().join("spool").exists());
}

#[test]
fn expired_local_fallback_does_not_load_or_create_board_storage() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let args = ["--no-daemon", "board", "show"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let options = crate::cli::parse(&args).unwrap();
    let forwarded = crate::board::prepare_client(&args, &options).unwrap();
    let options = crate::cli::parse(&forwarded).unwrap();
    let command = board_grammar::parse(&options, None).unwrap();
    let context = RequestContext::new(None, None);
    let mut transport = BoardClientTransport::default();
    let mut gateway = FakeGateway::default();
    let mut loads = 0;
    let mut load = || {
        loads += 1;
        Ok(BoardConfig::for_database(&database))
    };

    let error = transport
        .run_prepared(
            &forwarded,
            &options,
            &command,
            &context,
            &mut gateway,
            &mut load,
            None,
            &directory.path().join("spool"),
            &QueryDeadline::after(Duration::ZERO),
        )
        .unwrap_err();

    assert!(error.to_string().starts_with("timed_out:"), "{error}");
    assert_eq!(loads, 0);
    assert!(gateway.requests.is_empty());
    assert!(!database.exists());
    assert!(!directory.path().join("spool").exists());
}

#[test]
fn expired_router_ensure_spools_feedback_once_without_dispatch() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let summary = "startup deadline expired";
    let body = "Retain this exact report body.\nSecond line.\n";
    let args = feedback_args(directory.path(), summary, body);
    let mut gateway = FakeGateway {
        replies: VecDeque::from([Ok(None)]),
        ensure_delay: Duration::from_millis(750),
        ..Default::default()
    };
    let reply = invoke_args_with_load_delay(
        &args,
        &mut gateway,
        &mut BoardClientTransport::default(),
        &database,
        None,
        QueryDeadline::after(Duration::from_millis(500)),
        Duration::ZERO,
    )
    .unwrap();

    let reply_key = assert_queued_reply(&reply);
    assert_eq!(gateway.requests.len(), 1, "feedback must not be dispatched");
    assert_eq!(gateway.ensured, 1);
    assert_eq!(
        assert_queued_feedback(&directory.path().join("spool"), summary, body),
        reply_key
    );
    assert!(!database.exists());
}

#[test]
fn expired_status_probe_spools_feedback_once_without_dispatch() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let summary = "status probe deadline expired";
    let body = "The probe consumed the request budget.\n";
    let args = feedback_args(directory.path(), summary, body);
    let mut gateway = FakeGateway {
        replies: VecDeque::from([Ok(Some(capability_status(&database)))]),
        request_delay: Duration::from_millis(750),
        ..Default::default()
    };
    let reply = invoke_args_with_load_delay(
        &args,
        &mut gateway,
        &mut BoardClientTransport::default(),
        &database,
        None,
        QueryDeadline::after(Duration::from_millis(500)),
        Duration::ZERO,
    )
    .unwrap();

    let reply_key = assert_queued_reply(&reply);
    assert_eq!(gateway.requests.len(), 1, "feedback must not be dispatched");
    assert_eq!(gateway.ensured, 0);
    assert_eq!(
        assert_queued_feedback(&directory.path().join("spool"), summary, body),
        reply_key
    );
    assert!(!database.exists());
}

#[test]
fn expired_probe_preserves_board_api_mismatch_for_feedback() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let args = feedback_args(
        directory.path(),
        "router API mismatch",
        "Keep the capability error visible.\n",
    );
    let status = serde_json::json!({
        "status": "ok",
        "board_api": BOARD_API + 1,
        "board_db": database,
    })
    .to_string();
    let mut gateway = FakeGateway {
        replies: VecDeque::from([Ok(Some(status))]),
        request_delay: Duration::from_millis(750),
        ..Default::default()
    };
    let error = invoke_args_with_load_delay(
        &args,
        &mut gateway,
        &mut BoardClientTransport::default(),
        &database,
        None,
        QueryDeadline::after(Duration::from_millis(500)),
        Duration::ZERO,
    )
    .unwrap_err();

    assert!(
        error.to_string().starts_with("board_api_mismatch:"),
        "{error}"
    );
    assert_eq!(gateway.requests.len(), 1, "feedback must not be dispatched");
    assert!(!directory.path().join("spool").exists());
    assert!(!database.exists());
}

#[test]
fn expired_local_socket_access_check_spools_feedback_once() {
    let directory = scratch();
    let runtime = directory.path().join("runtime");
    let database = directory.path().join("board.sqlite3");
    let summary = "local access check deadline expired";
    let body = "Keep the report while checking router access.\n";
    let args = feedback_args(directory.path(), summary, body);
    let mut gateway = FakeGateway {
        replies: VecDeque::from([Ok(None)]),
        socket_denied: true,
        ..Default::default()
    };
    let reply = invoke_args_with_load_delay(
        &args,
        &mut gateway,
        &mut BoardClientTransport::default(),
        &database,
        Some(&runtime),
        QueryDeadline::after(Duration::from_millis(500)),
        Duration::from_millis(750),
    )
    .unwrap();

    let reply_key = assert_queued_reply(&reply);
    assert_eq!(gateway.requests.len(), 1, "feedback must not be dispatched");
    assert_eq!(gateway.ensured, 0);
    assert_eq!(
        assert_queued_feedback(&directory.path().join("spool"), summary, body),
        reply_key
    );
    assert!(!database.exists());
}

#[test]
fn feedback_lookups_use_the_caller_budget_then_a_fixed_allowance() {
    assert_eq!(
        crate::board::feedback_lookup_wait(&QueryDeadline::after(Duration::from_secs(10))),
        Duration::from_secs(2)
    );
    let short =
        crate::board::feedback_lookup_wait(&QueryDeadline::after(Duration::from_millis(100)));
    assert!(short <= Duration::from_millis(100), "{short:?}");
    assert_eq!(
        crate::board::feedback_lookup_wait(&QueryDeadline::after(Duration::ZERO)),
        Duration::from_millis(250)
    );
}
