use super::*;

#[test]
fn router_status_reports_supported_and_file_schema_versions() {
    let directory = tp_core::test_fixtures::scratch("board-status-");
    let database = directory.path().join("board.sqlite3");
    let executable = std::env::current_exe().unwrap();
    let config = tp_board::BoardConfig::for_database(&database);
    drop(tp_board::local_board::LocalBoard::open(&config).unwrap());
    let router = SystemRouter {
        runtime: Some(directory.path().join("runtime")),
        cache_base: None,
        sweeps: Mutex::new(SweepClock::default()),
        board: tp_board::BoardHost::with_config(config),
        lifecycle: Default::default(),
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
    assert_eq!(status["process_id"], serde_json::json!(std::process::id()));
    assert_eq!(
        status["executable"],
        serde_json::json!(executable.display().to_string())
    );
    assert_eq!(status["executable_replaced"], serde_json::json!(false));
    assert!(status["executable_error"].is_null());
    assert_eq!(
        status["lifecycle"],
        serde_json::json!({ "supervisor": "detached" })
    );
    assert_eq!(status["board_api"], tp_board::BOARD_API);
    assert_eq!(status["board_db"], serde_json::json!(database));
    assert_eq!(status["schema_supported"], tp_board::SCHEMA_VERSION);
    assert_eq!(status["schema_file"], tp_board::SCHEMA_VERSION);
}

#[test]
fn router_status_reports_migrated_schema_after_startup_migration() {
    let directory = tp_core::test_fixtures::scratch("board-status-");
    let database = directory.path().join("board.sqlite3");
    tp_board::local_board::seed_storage_schema(&database, tp_board::SCHEMA_VERSION - 1).unwrap();
    let config = tp_board::BoardConfig::for_database(&database);
    let board = tp_board::BoardHost::with_config(config);
    board.ensure_writer(QueryDeadline::start()).unwrap();
    let router = SystemRouter {
        runtime: Some(directory.path().join("runtime")),
        cache_base: None,
        sweeps: Mutex::new(SweepClock::default()),
        board,
        lifecycle: Default::default(),
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
    assert_eq!(status["schema_supported"], tp_board::SCHEMA_VERSION);
    assert_eq!(status["schema_file"], tp_board::SCHEMA_VERSION);
}

#[test]
fn router_status_omits_the_schema_file_of_an_absent_database() {
    let directory = tp_core::test_fixtures::scratch("board-status-");
    let database = directory.path().join("board.sqlite3");
    let router = SystemRouter {
        runtime: Some(directory.path().join("runtime")),
        cache_base: None,
        sweeps: Mutex::new(SweepClock::default()),
        board: tp_board::BoardHost::with_config(tp_board::BoardConfig::for_database(&database)),
        lifecycle: Default::default(),
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
    assert_eq!(status["schema_supported"], tp_board::SCHEMA_VERSION);
    assert!(status["schema_file"].is_null());
    assert!(!database.exists(), "status must not create the database");
}

#[test]
fn router_status_omits_board_fields_when_the_database_path_is_not_absolute() {
    let directory = tp_core::test_fixtures::scratch("board-status-");
    let runtime = directory.path().join("runtime");
    let router = SystemRouter {
        runtime: Some(runtime.clone()),
        cache_base: None,
        sweeps: Mutex::new(SweepClock::default()),
        board: tp_board::BoardHost::with_config(tp_board::BoardConfig::for_database(
            "relative.sqlite3",
        )),
        lifecycle: Default::default(),
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
    assert_eq!(status["board_api"], tp_board::BOARD_API);
    assert_eq!(status["schema_supported"], tp_board::SCHEMA_VERSION);
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
    let directory = tp_core::test_fixtures::scratch("board-status-");
    let runtime = directory.path().join("runtime");
    std::fs::create_dir(&runtime).unwrap();
    // A directory where the pin belongs makes the marker write fail on any uid.
    std::fs::create_dir(runtime.join("board-backend.json")).unwrap();
    let database = directory.path().join("board.sqlite3");
    let router = SystemRouter {
        runtime: Some(runtime.clone()),
        cache_base: None,
        sweeps: Mutex::new(SweepClock::default()),
        board: tp_board::BoardHost::with_config(tp_board::BoardConfig::for_database(&database)),
        lifecycle: Default::default(),
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
    assert_eq!(status["schema_supported"], tp_board::SCHEMA_VERSION);
    assert!(status["schema_file"].is_null());
    assert!(runtime.join("board-backend.json").is_dir());
}

#[test]
fn board_routes_without_workspace_or_owner_daemon() {
    let directory = tp_core::test_fixtures::scratch("board-transport-");
    let cache = directory.path().join("owner-cache");
    let database = directory.path().join("data/board.sqlite3");
    let router = SystemRouter {
        runtime: None,
        cache_base: None,
        sweeps: Mutex::new(SweepClock::default()),
        board: tp_board::BoardHost::with_config(tp_board::BoardConfig::for_database(&database)),
        lifecycle: Default::default(),
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
    let root = tp_core::test_fixtures::scratch("router-deadline-");
    let cache_directory = tp_core::test_fixtures::scratch("router-cache-");
    let cache = cache_directory.path().join("cache");
    let router = SystemRouter {
        runtime: None,
        cache_base: None,
        sweeps: Mutex::new(SweepClock::default()),
        board: tp_board::BoardHost::default(),
        lifecycle: Default::default(),
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
    assert!(
        tp_core::coded_error::ErrorCode::TimedOut.is_code_of(&error),
        "{error:#}"
    );
    assert!(!cache.exists(), "expired request spawned an owner daemon");
}
