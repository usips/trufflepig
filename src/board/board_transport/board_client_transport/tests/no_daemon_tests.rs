use super::*;

#[test]
fn no_daemon_board_access_never_contacts_or_starts_router() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    let mut gateway = FakeGateway::default();
    invoke(
        &["--no-daemon", "board", "show"],
        &mut gateway,
        &mut BoardClientTransport::default(),
        &database,
        None,
    )
    .unwrap();
    assert!(gateway.requests.is_empty());
    assert_eq!(gateway.ensured, 0);
    assert!(database.exists());
}

#[test]
fn no_daemon_refuses_to_migrate_beside_a_live_router() {
    let directory = scratch();
    let runtime = directory.path().join("runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    let _listener =
        std::os::unix::net::UnixListener::bind(runtime.join(crate::daemon::SOCKET_NAME)).unwrap();
    let database = directory.path().join("board.sqlite3");
    let error = invoke(
        &["--no-daemon", "board", "show"],
        &mut FakeGateway::default(),
        &mut BoardClientTransport::default(),
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
fn no_daemon_feedback_beside_live_router_is_queued() {
    let directory = scratch();
    let runtime = directory.path().join("runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    let _listener =
        std::os::unix::net::UnixListener::bind(runtime.join(crate::daemon::SOCKET_NAME)).unwrap();
    let database = directory.path().join("board.sqlite3");
    let reply = invoke(
        &["--no-daemon", "feedback", "blocked", "router live"],
        &mut FakeGateway::default(),
        &mut BoardClientTransport::default(),
        &database,
        Some(&runtime),
    )
    .unwrap();
    let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(reply["result"]["result"], "queued");
    let spool = directory.path().join("spool");
    let files: Vec<_> = std::fs::read_dir(&spool).unwrap().collect();
    assert_eq!(files.len(), 1);
    assert!(!database.exists());
}

#[test]
fn no_daemon_show_beside_live_router_succeeds_on_current_schema() {
    let directory = scratch();
    let database = directory.path().join("board.sqlite3");
    invoke(
        &["--no-daemon", "board", "new", "Seeded plan"],
        &mut FakeGateway::default(),
        &mut BoardClientTransport::default(),
        &database,
        None,
    )
    .unwrap();
    let runtime = directory.path().join("runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    let _listener =
        std::os::unix::net::UnixListener::bind(runtime.join(crate::daemon::SOCKET_NAME)).unwrap();
    let reply = invoke(
        &["--no-daemon", "board", "show"],
        &mut FakeGateway::default(),
        &mut BoardClientTransport::default(),
        &database,
        Some(&runtime),
    )
    .unwrap();
    assert!(reply.contains("Seeded plan"), "{reply}");
}
