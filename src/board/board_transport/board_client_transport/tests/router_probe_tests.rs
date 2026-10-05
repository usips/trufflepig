use super::*;

#[test]
fn silent_fallback_refuses_to_migrate_beside_a_live_router() {
    let directory = scratch();
    let runtime = directory.path().join("runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    let _listener =
        std::os::unix::net::UnixListener::bind(runtime.join(crate::daemon::SOCKET_NAME)).unwrap();
    let database = directory.path().join("board.sqlite3");
    let mut gateway = FakeGateway::default();
    let error = invoke(
        &["board", "show"],
        &mut gateway,
        &AtomicU64::new(0),
        &database,
        Some(&runtime),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("router owns migration"),
        "{error}"
    );
    assert!(!database.exists());
}

#[test]
fn router_api_is_probed_once_before_dispatch_and_mismatch_never_falls_back() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let status =
        serde_json::json!({"status":"ok","board_api":BOARD_API,"board_db":database}).to_string();
    let mut gateway = FakeGateway {
        replies: VecDeque::from([
            Ok(Some(status)),
            Ok(Some("first".into())),
            Ok(Some("second".into())),
        ]),
        ..Default::default()
    };
    let api = AtomicU64::new(0);
    assert_eq!(
        invoke(&["board", "show"], &mut gateway, &api, &database, None).unwrap(),
        "first"
    );
    assert_eq!(
        invoke(&["board", "show"], &mut gateway, &api, &database, None).unwrap(),
        "second"
    );
    assert_eq!(gateway.requests.len(), 3);
    assert_eq!(gateway.requests[0], ["system", "status"]);
    assert!(!database.exists());
    for stale in [4, BOARD_API + 1] {
        let status =
            serde_json::json!({"status":"ok","board_api":stale,"board_db":database}).to_string();
        let mut gateway = FakeGateway {
            replies: VecDeque::from([Ok(Some(status))]),
            ..Default::default()
        };
        let error = invoke(
            &["board", "new", "Never written"],
            &mut gateway,
            &AtomicU64::new(0),
            &database,
            None,
        )
        .unwrap_err();
        assert!(
            error.to_string().starts_with("board_api_mismatch:"),
            "{error}"
        );
        assert!(
            error
                .to_string()
                .contains("restart trufflepig-system.service"),
            "{error}"
        );
        assert_eq!(gateway.requests.len(), 1);
        assert_eq!(gateway.ensured, 0);
        assert!(!database.exists());
    }
}

#[test]
fn probe_without_board_db_surfaces_the_routers_board_error() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let status = serde_json::json!({
        "status": "ok",
        "board_api": BOARD_API,
        "board_error": "invalid_options: board database path must be absolute",
    })
    .to_string();
    let mut gateway = FakeGateway {
        replies: VecDeque::from([Ok(Some(status))]),
        ..Default::default()
    };
    let error = invoke(
        &["board", "show"],
        &mut gateway,
        &AtomicU64::new(0),
        &database,
        None,
    )
    .unwrap_err();
    assert!(
        error.to_string().starts_with("board_unavailable:"),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .contains("board database path must be absolute"),
        "{error}"
    );
    assert!(!database.exists());
}

#[test]
fn unavailable_marker_skips_ensure_without_extending_its_deadline() {
    let directory = scratch();
    let runtime = directory.path().join("runtime");
    let database = directory.path().join("board.sqlite3");
    let mut gateway = FakeGateway::default();
    let api = AtomicU64::new(0);
    invoke(
        &["board", "show"],
        &mut gateway,
        &api,
        &database,
        Some(&runtime),
    )
    .unwrap();
    let before = std::fs::read(runtime.join("board-router-unavailable.json")).unwrap();
    invoke(
        &["board", "show"],
        &mut gateway,
        &api,
        &database,
        Some(&runtime),
    )
    .unwrap();
    assert_eq!(gateway.ensured, 1);
    assert_eq!(
        std::fs::read(runtime.join("board-router-unavailable.json")).unwrap(),
        before
    );
    let expired = SystemTime::now() - Duration::from_secs(31);
    crate::system::mark_board_router_unavailable(&runtime, expired).unwrap();
    invoke(
        &["board", "show"],
        &mut gateway,
        &api,
        &database,
        Some(&runtime),
    )
    .unwrap();
    assert_eq!(gateway.ensured, 2);
}

#[test]
fn lost_router_write_reply_never_replays_or_sets_absence_marker() {
    let directory = scratch();
    let runtime = directory.path().join("runtime");
    let database = directory.path().join("board.sqlite3");
    let status =
        serde_json::json!({"status":"ok","board_api":BOARD_API,"board_db":database}).to_string();
    let mut gateway = FakeGateway {
        replies: VecDeque::from([
            Ok(Some(status)),
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe).into()),
        ]),
        ..Default::default()
    };
    invoke(
        &["board", "new", "Ambiguous write"],
        &mut gateway,
        &AtomicU64::new(0),
        &database,
        Some(&runtime),
    )
    .unwrap_err();
    assert_eq!(gateway.requests.len(), 2);
    assert!(!database.exists());
    assert!(!runtime.join("board-router-unavailable.json").exists());
}

#[test]
fn every_local_client_path_refuses_a_different_router_database_pin() {
    let directory = scratch();
    let runtime = directory.path().join("runtime");
    let database = directory.path().join("client.sqlite3");
    crate::system::record_board_database(&runtime, &directory.path().join("router.sqlite3"))
        .unwrap();
    for words in [vec!["--no-daemon", "board", "show"], vec!["board", "show"]] {
        let error = invoke(
            &words,
            &mut FakeGateway::default(),
            &AtomicU64::new(0),
            &database,
            Some(&runtime),
        )
        .unwrap_err();
        assert!(error.to_string().contains("differs from router database"));
        assert!(!database.exists());
    }
}

#[test]
fn failed_capability_probe_queues_feedback_once_with_its_stable_key() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let mut gateway = FakeGateway {
        replies: VecDeque::from([Err(
            std::io::Error::from(std::io::ErrorKind::BrokenPipe).into()
        )]),
        ..Default::default()
    };
    let reply = invoke(
        &["feedback", "blocked", "probe failed"],
        &mut gateway,
        &AtomicU64::new(0),
        &database,
        None,
    )
    .unwrap();
    let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(reply["result"]["result"], "queued");
    let key = reply["result"]["data"]["import_key"].as_str().unwrap();
    let spool = directory.path().join("spool");
    let files: Vec<_> = std::fs::read_dir(&spool).unwrap().collect();
    assert_eq!(files.len(), 1);
    let bytes = std::fs::read(spool.join(format!("{key}.feedback"))).unwrap();
    let record: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(record["import_key"], key);
    assert_eq!(record["request"]["op"]["import_key"], key);
    assert_eq!(gateway.requests.len(), 1);
    assert_eq!(gateway.ensured, 0);
    assert!(!database.exists());
}

#[test]
fn live_router_refuses_stale_schema_without_migrating() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    crate::board::local_board::seed_storage_schema(&database, crate::board::SCHEMA_VERSION - 1)
        .unwrap();
    let runtime = directory.path().join("runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    let _listener =
        std::os::unix::net::UnixListener::bind(runtime.join(crate::daemon::SOCKET_NAME)).unwrap();
    let error = invoke(
        &["--no-daemon", "board", "show"],
        &mut FakeGateway::default(),
        &AtomicU64::new(0),
        &database,
        Some(&runtime),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("router owns migration"),
        "{error}"
    );
    let version: i64 = rusqlite::Connection::open(&database)
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, crate::board::SCHEMA_VERSION - 1);
}
