use super::board_runtime::{BoardDatabaseMarker, board_database_marker_matches};
use super::*;

fn lookup<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
    move |name| {
        pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| OsString::from(value))
    }
}

fn no_login_session() -> Option<PathBuf> {
    None
}

#[test]
fn system_dir_override_wins() {
    let dir = dir_from(
        lookup(&[
            ("TRUFFLEPIG_SYSTEM_DIR", "/override"),
            ("XDG_RUNTIME_DIR", "/run/user"),
            ("XDG_CACHE_HOME", "/xdg-cache"),
            ("HOME", "/home"),
        ]),
        no_login_session,
    );
    assert_eq!(dir, Some(PathBuf::from("/override")));
}

#[test]
fn runtime_dir_beats_cache_and_home() {
    let dir = dir_from(
        lookup(&[
            ("XDG_RUNTIME_DIR", "/run/user"),
            ("XDG_CACHE_HOME", "/xdg-cache"),
            ("HOME", "/home"),
        ]),
        no_login_session,
    );
    assert_eq!(dir, Some(PathBuf::from("/run/user/trufflepig/system")));
}

#[test]
fn runtime_dir_beats_login_session() {
    let dir = dir_from(lookup(&[("XDG_RUNTIME_DIR", "/run/user")]), || {
        Some(PathBuf::from("/run/user/1000"))
    });
    assert_eq!(dir, Some(PathBuf::from("/run/user/trufflepig/system")));
}

#[test]
fn login_session_beats_cache_chain() {
    let dir = dir_from(
        lookup(&[("XDG_CACHE_HOME", "/xdg-cache"), ("HOME", "/home")]),
        || Some(PathBuf::from("/run/user/1000")),
    );
    assert_eq!(dir, Some(PathBuf::from("/run/user/1000/trufflepig/system")));
}

#[test]
fn cache_home_beats_home() {
    let dir = dir_from(
        lookup(&[("XDG_CACHE_HOME", "/xdg-cache"), ("HOME", "/home")]),
        no_login_session,
    );
    assert_eq!(dir, Some(PathBuf::from("/xdg-cache/trufflepig/system")));
}

#[test]
fn home_falls_back_to_dot_cache() {
    let dir = dir_from(lookup(&[("HOME", "/home")]), no_login_session);
    assert_eq!(dir, Some(PathBuf::from("/home/.cache/trufflepig/system")));
}

#[test]
fn no_base_directory_yields_none() {
    assert_eq!(dir_from(lookup(&[]), no_login_session), None);
}

#[test]
fn empty_override_falls_through_to_the_chain() {
    let dir = dir_from(
        lookup(&[("TRUFFLEPIG_SYSTEM_DIR", ""), ("HOME", "/home")]),
        no_login_session,
    );
    assert_eq!(dir, Some(PathBuf::from("/home/.cache/trufflepig/system")));
}

#[test]
fn empty_runtime_dir_falls_through_to_cache_chain() {
    let dir = dir_from(
        lookup(&[
            ("XDG_RUNTIME_DIR", ""),
            ("XDG_CACHE_HOME", "/xdg-cache"),
            ("HOME", "/home"),
        ]),
        no_login_session,
    );
    assert_eq!(dir, Some(PathBuf::from("/xdg-cache/trufflepig/system")));
}

#[test]
fn relative_values_fall_through_to_home() {
    let dir = dir_from(
        lookup(&[
            ("TRUFFLEPIG_SYSTEM_DIR", "relative/override"),
            ("XDG_RUNTIME_DIR", "relative/runtime"),
            ("XDG_CACHE_HOME", "relative/cache"),
            ("HOME", "/home"),
        ]),
        no_login_session,
    );
    assert_eq!(dir, Some(PathBuf::from("/home/.cache/trufflepig/system")));
}

#[test]
fn spool_dir_override_wins() {
    let dir = spool_dir_from(lookup(&[("TRUFFLEPIG_SPOOL_DIR", "/override")]), 1000);
    assert_eq!(dir, PathBuf::from("/override"));
}

#[test]
fn spool_dir_defaults_to_per_user_tmp() {
    let dir = spool_dir_from(lookup(&[("XDG_RUNTIME_DIR", "/run/user")]), 1000);
    assert_eq!(dir, PathBuf::from("/tmp/trufflepig-1000/spool"));
}

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

#[test]
fn router_status_reports_supported_and_file_schema_versions() {
    let directory = crate::board::board_test_support::scratch("board-status-");
    let database = directory.path().join("board.sqlite3");
    let config = crate::board::BoardConfig::for_database(&database);
    drop(crate::board::local_board::LocalBoard::open(&config).unwrap());
    let router = SystemRouter {
        runtime: Some(directory.path().join("runtime")),
        cache_base: None,
        sweeps: Mutex::new(SweepClock::default()),
        board: crate::board::BoardHost::with_config(config),
    };
    let reply = router
        .request(AcceptedRequest {
            args: ["system".into(), "status".into()].into(),
            context: RequestContext::new(None, None),
            deadline: QueryDeadline::start(),
        })
        .unwrap();
    let status: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(status["status"], "ok");
    assert_eq!(status["board_api"], crate::board::BOARD_API);
    assert_eq!(status["board_db"], serde_json::json!(database));
    assert_eq!(status["schema_supported"], crate::board::SCHEMA_VERSION);
    assert_eq!(status["schema_file"], crate::board::SCHEMA_VERSION);
}

#[test]
fn router_status_reports_migrated_schema_after_startup_migration() {
    let directory = crate::board::board_test_support::scratch("board-status-");
    let database = directory.path().join("board.sqlite3");
    crate::board::local_board::seed_storage_schema(
        &database,
        crate::board::SCHEMA_VERSION - 1,
    )
    .unwrap();
    let config = crate::board::BoardConfig::for_database(&database);
    let board = crate::board::BoardHost::with_config(config);
    board.ensure_writer(QueryDeadline::start()).unwrap();
    let router = SystemRouter {
        runtime: Some(directory.path().join("runtime")),
        cache_base: None,
        sweeps: Mutex::new(SweepClock::default()),
        board,
    };
    let reply = router
        .request(AcceptedRequest {
            args: ["system".into(), "status".into()].into(),
            context: RequestContext::new(None, None),
            deadline: QueryDeadline::start(),
        })
        .unwrap();
    let status: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(status["status"], "ok");
    assert_eq!(status["schema_supported"], crate::board::SCHEMA_VERSION);
    assert_eq!(status["schema_file"], crate::board::SCHEMA_VERSION);
}

#[test]
fn router_status_omits_the_schema_file_of_an_absent_database() {
    let directory = crate::board::board_test_support::scratch("board-status-");
    let database = directory.path().join("board.sqlite3");
    let router = SystemRouter {
        runtime: Some(directory.path().join("runtime")),
        cache_base: None,
        sweeps: Mutex::new(SweepClock::default()),
        board: crate::board::BoardHost::with_config(crate::board::BoardConfig::for_database(
            &database,
        )),
    };
    let reply = router
        .request(AcceptedRequest {
            args: ["system".into(), "status".into()].into(),
            context: RequestContext::new(None, None),
            deadline: QueryDeadline::start(),
        })
        .unwrap();
    let status: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(status["status"], "ok");
    assert_eq!(status["board_db"], serde_json::json!(database));
    assert_eq!(status["schema_supported"], crate::board::SCHEMA_VERSION);
    assert!(status["schema_file"].is_null());
    assert!(!database.exists(), "status must not create the database");
}

#[test]
fn router_status_omits_board_fields_when_the_database_path_is_not_absolute() {
    let directory = crate::board::board_test_support::scratch("board-status-");
    let runtime = directory.path().join("runtime");
    let router = SystemRouter {
        runtime: Some(runtime.clone()),
        cache_base: None,
        sweeps: Mutex::new(SweepClock::default()),
        board: crate::board::BoardHost::with_config(crate::board::BoardConfig::for_database(
            "relative.sqlite3",
        )),
    };
    let reply = router
        .request(AcceptedRequest {
            args: ["system".into(), "status".into()].into(),
            context: RequestContext::new(None, None),
            deadline: QueryDeadline::start(),
        })
        .unwrap();
    let status: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(status["status"], "ok");
    assert_eq!(status["board_api"], crate::board::BOARD_API);
    assert_eq!(status["schema_supported"], crate::board::SCHEMA_VERSION);
    assert!(status["board_db"].is_null());
    assert!(status["schema_file"].is_null());
    assert!(!runtime.exists(), "a failed pin must not create its directory");
    let error = router
        .request(AcceptedRequest {
            args: ["board".into(), "show".into()].into(),
            context: RequestContext::new(None, None),
            deadline: QueryDeadline::start(),
        })
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("invalid_options: board database path must be absolute"),
        "{error:#}"
    );
}

#[test]
fn router_status_survives_a_database_pin_write_failure() {
    let directory = crate::board::board_test_support::scratch("board-status-");
    let runtime = directory.path().join("runtime");
    std::fs::create_dir(&runtime).unwrap();
    // A directory where the pin belongs makes the marker write fail on any uid.
    std::fs::create_dir(runtime.join("board-backend.json")).unwrap();
    let database = directory.path().join("board.sqlite3");
    let router = SystemRouter {
        runtime: Some(runtime.clone()),
        cache_base: None,
        sweeps: Mutex::new(SweepClock::default()),
        board: crate::board::BoardHost::with_config(crate::board::BoardConfig::for_database(
            &database,
        )),
    };
    let reply = router
        .request(AcceptedRequest {
            args: ["system".into(), "status".into()].into(),
            context: RequestContext::new(None, None),
            deadline: QueryDeadline::start(),
        })
        .unwrap();
    let status: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(status["status"], "ok");
    assert_eq!(status["board_db"], serde_json::json!(database));
    assert_eq!(status["schema_supported"], crate::board::SCHEMA_VERSION);
    assert!(status["schema_file"].is_null());
    assert!(runtime.join("board-backend.json").is_dir());
}

#[test]
fn ensure_reports_a_running_router_despite_unresolvable_board_database() {
    if std::env::var_os("TRUFFLEPIG_SYSTEM_TEST_RELATIVE_DB").is_none() {
        // The child process owns the board environment; parallel tests never see it.
        let directory = crate::board::board_test_support::scratch("board-lazy-");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .env("TRUFFLEPIG_SYSTEM_TEST_RELATIVE_DB", "1")
            .env("TRUFFLEPIG_BOARD_DB", "relative.sqlite3")
            .env("TRUFFLEPIG_SYSTEM_DIR", directory.path().join("runtime"))
            .env("TRUFFLEPIG_SPOOL_DIR", directory.path().join("spool"))
            .args([
                "system::tests::ensure_reports_a_running_router_despite_unresolvable_board_database",
                "--exact",
                "--nocapture",
            ])
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let runtime = PathBuf::from(std::env::var_os("TRUFFLEPIG_SYSTEM_DIR").unwrap());
    let spool = PathBuf::from(std::env::var_os("TRUFFLEPIG_SPOOL_DIR").unwrap());
    let router = SystemRouter {
        runtime: Some(runtime.clone()),
        cache_base: None,
        sweeps: Mutex::new(SweepClock::default()),
        board: crate::board::BoardHost::default(),
    };
    let worker_runtime = runtime.clone();
    let worker = std::thread::spawn(move || daemon::serve_router(&worker_runtime, &spool, router));
    let ping: Vec<String> = ["system".into(), "status".into()].into();
    let context = RequestContext::new(None, None);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if daemon::request(&runtime, &ping, &context).unwrap().is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "router failed to start");
        std::thread::sleep(Duration::from_millis(10));
    }
    let reply = crate::system::request(&ping, &context).unwrap().unwrap();
    let status: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(status["status"], "ok");
    assert_eq!(status["board_api"], crate::board::BOARD_API);
    assert_eq!(status["schema_supported"], crate::board::SCHEMA_VERSION);
    assert!(status["board_db"].is_null());
    assert!(status["schema_file"].is_null());
    assert!(
        status["board_error"]
            .as_str()
            .is_some_and(|error| error.contains("board database path must be absolute")),
        "{status}"
    );
    crate::system::ensure().unwrap();
    let error = crate::system::request(&["board".into(), "show".into()], &context).unwrap_err();
    assert!(
        format!("{error:#}").contains("board database path must be absolute"),
        "{error:#}"
    );
    daemon::stop(&runtime).unwrap();
    worker.join().unwrap().unwrap();
}

#[test]
fn board_routes_without_workspace_or_owner_daemon() {
    let directory = crate::board::board_test_support::scratch("board-transport-");
    let cache = directory.path().join("owner-cache");
    let database = directory.path().join("data/board.sqlite3");
    let router = SystemRouter {
        runtime: None,
        cache_base: None,
        sweeps: Mutex::new(SweepClock::default()),
        board: crate::board::BoardHost::with_config(crate::board::BoardConfig::for_database(
            &database,
        )),
    };
    let args: Vec<String> = [
        "--root",
        directory.path().join("missing-root").to_str().unwrap(),
        "--workspace",
        directory
            .path()
            .join("missing-workspace.toml")
            .to_str()
            .unwrap(),
        "board",
        "show",
    ]
    .map(str::to_owned)
    .into();
    let reply = router
        .request(AcceptedRequest {
            args,
            context: RequestContext::new(None, None),
            deadline: QueryDeadline::start(),
        })
        .unwrap();
    let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(reply["result"]["result"], "overview");
    assert!(database.exists());
    assert!(!cache.exists(), "board routing spawned an owner daemon");
}

#[test]
fn router_does_not_reset_an_expired_accepted_deadline() {
    let root = crate::board::board_test_support::scratch("router-deadline-");
    let cache_directory = crate::board::board_test_support::scratch("router-cache-");
    let cache = cache_directory.path().join("cache");
    let router = SystemRouter {
        runtime: None,
        cache_base: None,
        sweeps: Mutex::new(SweepClock::default()),
        board: crate::board::BoardHost::default(),
    };
    let args: Vec<String> = [
        "--no-workspace",
        "--root",
        root.path().to_str().unwrap(),
        "--cache",
        cache.to_str().unwrap(),
        "search",
        "bounded",
    ]
    .map(str::to_owned)
    .into();

    let error = router
        .request(AcceptedRequest {
            context: RequestContext::new(None, None),
            args,
            deadline: QueryDeadline::after(Duration::ZERO),
        })
        .unwrap_err();
    assert!(crate::daemon::deadline::is_timed_out(&error), "{error:#}");
    assert!(!cache.exists(), "expired request spawned an owner daemon");
}
