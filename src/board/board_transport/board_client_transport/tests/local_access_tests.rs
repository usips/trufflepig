use super::*;

#[test]
fn denied_router_socket_reports_paths_without_starting_or_opening_local_board() {
    let directory = scratch();
    let runtime = directory.path().join("runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    let database = directory.path().join("board.sqlite3");
    let mut gateway = FakeGateway {
        socket_denied: true,
        ..Default::default()
    };

    let error = invoke(
        &["board", "show"],
        &mut gateway,
        &mut BoardClientTransport::default(),
        &database,
        Some(&runtime),
    )
    .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains(
            &runtime
                .join(crate::daemon::SOCKET_NAME)
                .display()
                .to_string()
        )
    );
    assert!(message.contains(&database.display().to_string()));
    assert!(message.contains("create AF_UNIX client sockets and connect"));
    assert!(message.contains("parent-directory traversal"));
    assert!(message.contains("current Unix identity"));
    assert!(!message.contains("system ensure"));
    assert_eq!(gateway.requests.len(), 1);
    assert_eq!(gateway.ensured, 0);
    assert!(!database.exists());
}

#[test]
fn denied_router_socket_after_probe_does_not_fall_back_to_a_local_database() {
    let directory = scratch();
    let runtime = directory.path().join("runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    let database = directory.path().join("board.sqlite3");
    let status =
        serde_json::json!({"status":"ok","board_api":BOARD_API,"board_db":database}).to_string();
    let mut gateway = FakeGateway {
        replies: VecDeque::from([Ok(Some(status)), Ok(None)]),
        socket_denied: true,
        ..Default::default()
    };

    let error = invoke(
        &["board", "show"],
        &mut gateway,
        &mut BoardClientTransport::default(),
        &database,
        Some(&runtime),
    )
    .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains(
            &runtime
                .join(crate::daemon::SOCKET_NAME)
                .display()
                .to_string()
        )
    );
    assert!(message.contains(&database.display().to_string()));
    assert_eq!(gateway.requests.len(), 2);
    assert_eq!(gateway.ensured, 0);
    assert!(!database.exists());
}

#[test]
fn readonly_cantopen_and_permission_errors_report_local_storage_paths() {
    let directory = scratch();
    let runtime = directory.path().join("runtime");
    let database = directory.path().join("board.sqlite3");
    let config = BoardConfig::for_database(&database);
    let socket = runtime.join(crate::daemon::SOCKET_NAME);

    for detail in [
        "attempt to write a readonly database",
        "unable to open database file",
        "database directory is not writable",
        "Permission denied (os error 13)",
    ] {
        let error = anyhow::Error::new(crate::board::board_protocol::BoardError::new(
            crate::board::board_protocol::BoardErrorCode::BoardUnavailable,
            detail,
        ));
        assert!(super::super::local_access::is_storage_access_failure(
            &error
        ));
        let message =
            super::super::local_access::storage_access_advice(error, &config, Some(&runtime))
                .to_string();
        assert!(
            message.contains(&database.display().to_string()),
            "{message}"
        );
        assert!(message.contains(&socket.display().to_string()), "{message}");
        assert!(message.contains("directory traversal"), "{message}");
        assert!(message.contains("current Unix identity"), "{message}");
        assert!(!message.contains("system ensure"), "{message}");
    }

    let unrelated = anyhow::Error::new(crate::board::board_protocol::BoardError::new(
        crate::board::board_protocol::BoardErrorCode::BoardUnavailable,
        "database is corrupt",
    ));
    assert!(!super::super::local_access::is_storage_access_failure(
        &unrelated
    ));
}

#[test]
fn local_readonly_database_after_unreachable_router_reports_access_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let directory = scratch();
    let runtime = directory.path().join("runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    let database = directory.path().join("board.sqlite3");
    crate::board::local_board::seed_storage_schema(&database, crate::board::SCHEMA_VERSION)
        .unwrap();
    std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o400)).unwrap();
    let mut gateway = FakeGateway::default();

    let error = invoke(
        &["board", "new", "Cannot write this"],
        &mut gateway,
        &mut BoardClientTransport::default(),
        &database,
        Some(&runtime),
    )
    .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains(&database.display().to_string()),
        "{message}"
    );
    assert!(
        message.contains(
            &runtime
                .join(crate::daemon::SOCKET_NAME)
                .display()
                .to_string()
        ),
        "{message}"
    );
    assert!(
        message.contains("database file is not writable"),
        "{message}"
    );
    assert!(message.contains("directory traversal"), "{message}");
    assert!(
        !message.contains("run trufflepig system ensure"),
        "{message}"
    );
}

#[test]
fn local_readonly_database_directory_after_unreachable_router_reports_access_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let directory = scratch();
    let runtime = directory.path().join("runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    let database_dir = directory.path().join("board-data");
    std::fs::create_dir(&database_dir).unwrap();
    std::fs::set_permissions(&database_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let database = database_dir.join("board.sqlite3");
    crate::board::local_board::seed_storage_schema(&database, crate::board::SCHEMA_VERSION)
        .unwrap();
    std::fs::set_permissions(&database_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let mut gateway = FakeGateway::default();

    let result = invoke(
        &["board", "new", "Cannot write this"],
        &mut gateway,
        &mut BoardClientTransport::default(),
        &database,
        Some(&runtime),
    );
    std::fs::set_permissions(&database_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let error = result.unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains(&database.display().to_string()),
        "{message}"
    );
    assert!(
        message.contains(
            &runtime
                .join(crate::daemon::SOCKET_NAME)
                .display()
                .to_string()
        ),
        "{message}"
    );
    assert!(
        message.contains("database directory is not writable"),
        "{message}"
    );
    assert!(
        message.contains("parent-directory write access"),
        "{message}"
    );
    assert!(
        !message.contains("run trufflepig system ensure"),
        "{message}"
    );
}
