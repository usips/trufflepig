use super::*;

#[test]
fn read_connection_refuses_to_create_or_migrate_storage() {
    let directory = directory();
    let missing = directory.path().join("missing.sqlite3");
    assert!(open_read_with_timeout(&missing, Duration::from_secs(1)).is_err());
    assert!(!missing.exists());
    let path = directory.path().join("legacy.sqlite3");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(SCHEMA_V1).unwrap();
    conn.pragma_update(None, "user_version", 1).unwrap();
    drop(conn);
    assert!(open_read_with_timeout(&path, Duration::from_secs(1)).is_err());
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[cfg(unix)]
#[test]
fn writable_open_refuses_a_group_or_world_accessible_parent_without_touching_it() {
    use std::os::unix::fs::PermissionsExt;
    let directory = directory();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = directory.path().join("board.sqlite3");
    let error = match open(&path) {
        Err(error) => error,
        Ok(_) => panic!("opened a database in a group/world-accessible directory"),
    };
    assert_eq!(
        error.code,
        crate::board::board_protocol::BoardErrorCode::BoardUnavailable
    );
    assert!(
        error.message.contains("chmod 0700"),
        "error must tell the user to move the database or tighten it themselves: {}",
        error.message
    );
    assert_eq!(
        directory.path().metadata().unwrap().permissions().mode() & 0o777,
        0o755,
        "a pre-existing directory is never chmodded"
    );
    assert!(!path.exists());
}

#[cfg(unix)]
#[test]
fn writable_open_accepts_a_preexisting_private_parent() {
    use std::os::unix::fs::PermissionsExt;
    let directory = directory();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let (conn, _) = open(&directory.path().join("board.sqlite3")).unwrap();
    drop(conn);
    assert_eq!(
        directory.path().metadata().unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[cfg(unix)]
#[test]
fn writable_open_tightens_only_directories_it_created() {
    use std::os::unix::fs::PermissionsExt;
    let directory = directory();
    let shared = directory.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();
    let data = shared.join("data");
    let nested = shared.join("nested/board");
    for parent in [&data, &nested] {
        let (conn, _) = open(&parent.join("board.sqlite3")).unwrap();
        drop(conn);
        assert_eq!(
            parent.metadata().unwrap().permissions().mode() & 0o777,
            0o700,
            "board-created data directory is tightened"
        );
    }
    assert_eq!(
        nested
            .parent()
            .unwrap()
            .metadata()
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700,
        "board-created intermediate directories are tightened"
    );
    assert_eq!(
        shared.metadata().unwrap().permissions().mode() & 0o777,
        0o755,
        "pre-existing directories keep their modes"
    );
}

#[cfg(unix)]
#[test]
fn readonly_open_never_changes_directory_modes() {
    use std::os::unix::fs::PermissionsExt;
    let directory = directory();
    let path = directory.path().join("board.sqlite3");
    drop(open(&path).unwrap());
    for mode in [0o755, 0o500] {
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(mode)).unwrap();
        let (conn, _) = open_read_with_timeout(&path, Duration::from_secs(1)).unwrap();
        assert_eq!(conn.total_changes(), 0);
        drop(conn);
        assert_eq!(
            directory.path().metadata().unwrap().permissions().mode() & 0o777,
            mode,
            "read paths must not modify the filesystem"
        );
    }
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
}
