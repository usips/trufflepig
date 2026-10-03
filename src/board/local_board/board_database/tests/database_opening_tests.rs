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
fn writable_open_tightens_existing_database_directory() {
    use std::os::unix::fs::PermissionsExt;
    let directory = directory();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let (conn, _) = open(&directory.path().join("board.sqlite3")).unwrap();
    drop(conn);
    assert_eq!(
        directory.path().metadata().unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[cfg(unix)]
#[test]
fn readonly_open_tightens_private_directory_without_enabling_owner_writes() {
    use std::os::unix::fs::PermissionsExt;
    let directory = directory();
    let path = directory.path().join("board.sqlite3");
    drop(open(&path).unwrap());
    for mode in [0o755, 0o555] {
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(mode)).unwrap();
        let (conn, _) = open_read_with_timeout(&path, Duration::from_secs(1)).unwrap();
        assert_eq!(
            directory.path().metadata().unwrap().permissions().mode() & 0o777,
            mode & !0o077
        );
        assert_eq!(conn.total_changes(), 0);
        drop(conn);
    }
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
}
