use super::*;

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
    crate::board::local_board::seed_storage_schema(&database, crate::board::SCHEMA_VERSION - 1)
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
    assert!(
        !runtime.exists(),
        "a failed pin must not create its directory"
    );
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
