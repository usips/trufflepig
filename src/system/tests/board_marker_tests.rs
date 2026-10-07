use super::*;

#[test]
fn unchanged_private_database_marker_preserves_inode_and_mtime() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::time::{Duration, SystemTime};
    let directory = crate::board::board_test_support::scratch("board-marker-idempotence-");
    let first_database = directory.path().join("first.sqlite3");
    super::record_board_database(directory.path(), &first_database).unwrap();
    let marker = directory.path().join("board-backend.json");
    let old_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
    std::fs::File::open(&marker)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(old_time))
        .unwrap();
    let before = marker.metadata().unwrap();
    super::record_board_database(directory.path(), &first_database).unwrap();
    let unchanged = marker.metadata().unwrap();
    assert_eq!(unchanged.ino(), before.ino());
    assert_eq!(unchanged.modified().unwrap(), before.modified().unwrap());
    assert_eq!(unchanged.permissions().mode() & 0o7777, 0o600);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);

    let second_database = directory.path().join("second.sqlite3");
    super::record_board_database(directory.path(), &second_database).unwrap();
    let updated = marker.metadata().unwrap();
    assert_ne!(updated.ino(), before.ino());
    let marker: BoardDatabaseMarker =
        serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
    assert_eq!(marker.database, second_database);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn database_marker_rejects_symlinks_nonprivate_files_and_other_owners() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let directory = crate::board::board_test_support::scratch("board-marker-security-");
    let database = directory.path().join("board.sqlite3");
    let marker = directory.path().join("board-backend.json");
    let outside = directory.path().join("outside");
    std::fs::write(&outside, b"private outside data").unwrap();
    std::os::unix::fs::symlink(&outside, &marker).unwrap();
    assert!(super::record_board_database(directory.path(), &database).is_err());
    assert!(marker.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read(&outside).unwrap(), b"private outside data");
    std::fs::remove_file(&marker).unwrap();

    super::record_board_database(directory.path(), &database).unwrap();
    let bytes = std::fs::read(&marker).unwrap();
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(super::record_board_database(directory.path(), &database).is_err());
    assert_eq!(std::fs::read(&marker).unwrap(), bytes);
    assert_eq!(
        marker.metadata().unwrap().permissions().mode() & 0o7777,
        0o644
    );
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600)).unwrap();
    let actual_owner = marker.metadata().unwrap().uid();
    assert!(board_database_marker_matches(&marker, &bytes, actual_owner.wrapping_add(1)).is_err());
    assert_eq!(std::fs::read(&marker).unwrap(), bytes);

    std::fs::remove_file(&marker).unwrap();
    std::fs::create_dir(&marker).unwrap();
    assert!(super::record_board_database(directory.path(), &database).is_err());
    assert!(marker.is_dir());
}

#[test]
fn losing_router_start_cannot_replace_the_live_database_marker() {
    let directory = crate::board::board_test_support::scratch("board-runtime-");
    let runtime = directory.path().join("runtime");
    let spool = directory.path().join("spool");
    let live_database = directory.path().join("live.sqlite3");
    let loser_database = directory.path().join("loser.sqlite3");
    let router = |database: &Path| SystemRouter {
        runtime: Some(runtime.clone()),
        cache_base: None,
        sweeps: Mutex::new(SweepClock::default()),
        board: crate::board::BoardHost::with_config(crate::board::BoardConfig::for_database(
            database,
        )),
    };
    let live = router(&live_database);
    let live_runtime = runtime.clone();
    let live_spool = spool.clone();
    let worker = std::thread::spawn(move || daemon::serve_router(&live_runtime, &live_spool, live));
    let deadline = Instant::now() + Duration::from_secs(3);
    let ping = ["system".into(), "status".into()];
    let context = RequestContext::new(None, None);
    loop {
        if daemon::request(&runtime, &ping, &context)
            .unwrap()
            .is_some()
        {
            break;
        }
        assert!(Instant::now() < deadline, "live router failed to start");
        std::thread::sleep(Duration::from_millis(10));
    }
    let rejected = daemon::serve_router(&runtime, &spool, router(&loser_database));
    assert!(rejected.is_err());
    let accepted = validate_board_database(&runtime, &live_database);
    let refused = validate_board_database(&runtime, &loser_database);
    daemon::stop(&runtime).unwrap();
    worker.join().unwrap().unwrap();
    accepted.unwrap();
    assert!(refused.is_err());
    assert!(!loser_database.exists());
}

#[test]
fn router_database_marker_refuses_split_local_fallback() {
    let directory = crate::board::board_test_support::scratch("board-runtime-");
    let pinned = directory.path().join("router.sqlite3");
    record_board_database(directory.path(), &pinned).unwrap();
    validate_board_database(directory.path(), &pinned).unwrap();
    let other = directory.path().join("client.sqlite3");
    let error = validate_board_database(directory.path(), &other).unwrap_err();
    assert!(error.to_string().contains("differs from router database"));
    assert!(!other.exists());
}
