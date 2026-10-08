use super::*;
use crate::board::board_protocol::ReadScope;

#[test]
fn panicked_board_write_rolls_back_then_reopens_for_durable_writes() {
    let directory = crate::board::board_test_support::scratch("board-runtime-");
    let database = directory.path().join("board.sqlite3");
    let host = BoardHost::with_config(BoardConfig::for_database(&database));
    let actor = host
        .config()
        .unwrap()
        .actor(None, Some("panic-test"))
        .unwrap();
    let create = |title: &str| {
        BoardRequest::new(
            actor.clone(),
            BoardOp::New {
                title: crate::board::board_vocabulary::PlanTitle::new(title).unwrap(),
                body: crate::board::board_vocabulary::PlanText::new("").unwrap(),
                steward: None,
                repo_key: None,
            },
        )
    };
    host.handle_by(&create("Before panic"), QueryDeadline::start())
        .unwrap();
    host.inner
        .backend
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .inject_panic_after_write();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        host.handle_by(&create("Rolled back"), QueryDeadline::start())
            .unwrap();
    }));
    assert!(panic.is_err());
    assert!(host.inner.backend.is_poisoned());
    host.handle_by(&create("After recovery"), QueryDeadline::start())
        .unwrap();
    assert!(!host.inner.backend.is_poisoned());
    let durable = rusqlite::Connection::open(database).unwrap();
    let titles = durable
        .prepare("SELECT title FROM plans ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(titles, ["Before panic", "After recovery"]);
    assert_eq!(
        durable
            .query_row("SELECT COUNT(*) FROM events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

fn poison<T>(mutex: &Mutex<T>) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = mutex.lock().unwrap();
        panic!("injected board worker panic");
    }));
    assert!(mutex.is_poisoned());
}

#[test]
fn poisoned_writer_reopens_durable_board_and_recovers_other_locks() {
    let directory = crate::board::board_test_support::scratch("board-test-");
    let host = BoardHost::with_config(BoardConfig::for_database(
        directory.path().join("board.sqlite3"),
    ));
    let context = RequestContext::new(None, None);
    let create =
        crate::cli::parse(&["board".into(), "new".into(), "Survives panic".into()]).unwrap();
    host.run(&create, &context, QueryDeadline::start()).unwrap();
    poison(&host.inner.backend);
    poison(&host.inner.config);
    poison(&host.inner.sequence);
    poison(&host.inner.ingestor);
    let show = crate::cli::parse(&["board".into(), "show".into()]).unwrap();
    let reply = host.run(&show, &context, QueryDeadline::start()).unwrap();
    assert!(reply.contains("Survives panic"));
    let create =
        crate::cli::parse(&["board".into(), "new".into(), "Recovered writer".into()]).unwrap();
    host.run(&create, &context, QueryDeadline::start()).unwrap();
    drop(lock_before(&host.inner.ingestor, QueryDeadline::start(), "ingest").unwrap());
    assert!(!host.inner.backend.is_poisoned());
    assert!(!host.inner.config.is_poisoned());
    assert!(!host.inner.sequence.is_poisoned());
    assert!(!host.inner.ingestor.is_poisoned());
}

#[test]
fn host_reads_existing_board_without_initializing_or_waiting_for_writer() {
    use std::os::unix::fs::PermissionsExt;
    let directory = crate::board::board_test_support::scratch("board-runtime-");
    let parent = directory.path().join("data");
    std::fs::create_dir(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = BoardConfig::for_database(parent.join("board.sqlite3"));
    let actor = config.actor(None, Some("existing-read")).unwrap();
    {
        let mut original = LocalBoard::open(&config).unwrap();
        original
            .handle(&BoardRequest::new(
                actor.clone(),
                BoardOp::New {
                    title: crate::board::board_vocabulary::PlanTitle::new("Readable board")
                        .unwrap(),
                    body: crate::board::board_vocabulary::PlanText::new("").unwrap(),
                    steward: None,
                    repo_key: None,
                },
            ))
            .unwrap();
    }
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o500)).unwrap();
    let host = BoardHost::with_config(config);
    let held = host.inner.backend.lock().unwrap();
    let request = BoardRequest::new(
        actor,
        BoardOp::Overview {
            scope: ReadScope::All,
            after: None,
            through: None,
            limit: 200,
        },
    );
    let started = Instant::now();
    let reply = host.handle_by(&request, QueryDeadline::after(Duration::from_secs(1)));
    drop(held);
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    let reply = reply.unwrap();
    let BoardResult::Overview(overview) = reply.result else {
        panic!("expected stored plans");
    };
    assert_eq!(overview.plans[0].plan.title.as_str(), "Readable board");
    assert!(host.inner.backend.lock().unwrap().is_none());
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[test]
fn expired_board_mutation_never_creates_the_database() {
    let directory = crate::board::board_test_support::scratch("board-transport-");
    let database = directory.path().join("data/board.sqlite3");
    let host = BoardHost::with_config(BoardConfig::for_database(&database));
    let options =
        crate::cli::parse(&["board".into(), "new".into(), "Never created".into()]).unwrap();
    let error = host
        .run(
            &options,
            &RequestContext::new(None, None),
            QueryDeadline::after(Duration::ZERO),
        )
        .unwrap_err();
    assert!(crate::daemon::deadline::is_timed_out(&error));
    assert!(!database.exists());
}

#[test]
fn writer_lock_wait_respects_the_accepted_deadline() {
    let writer = Mutex::new(());
    let held = writer.lock().unwrap();
    let started = Instant::now();
    let result = lock_before(
        &writer,
        QueryDeadline::after(Duration::from_millis(20)),
        "writer",
    );
    assert!(crate::daemon::deadline::is_timed_out(&result.unwrap_err()));
    assert!(started.elapsed() < Duration::from_millis(150));
    drop(held);
}

#[test]
fn host_bootstraps_writable_legacy_storage_but_keeps_readonly_initialization_error() {
    use std::os::unix::fs::PermissionsExt;
    let directory = crate::board::board_test_support::scratch("legacy-host-");
    let config = BoardConfig::for_database(directory.path().join("board.sqlite3"));
    let external = rusqlite::Connection::open(&config.db_path).unwrap();
    external.pragma_update(None, "user_version", 0).unwrap();
    std::fs::set_permissions(&config.db_path, std::fs::Permissions::from_mode(0o400)).unwrap();
    let host = BoardHost::with_config(config.clone());
    let request = BoardRequest::new(
        config.actor(None, Some("legacy-reader")).unwrap(),
        BoardOp::Overview {
            scope: ReadScope::All,
            after: None,
            through: None,
            limit: 200,
        },
    );
    let error = host
        .handle_by(&request, QueryDeadline::start())
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<BoardError>().map(|error| error.code),
        Some(crate::board::board_protocol::BoardErrorCode::BoardInitializationRequired)
    );
    assert!(host.inner.backend.lock().unwrap().is_none());
    std::fs::set_permissions(&config.db_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    host.handle_by(&request, QueryDeadline::start()).unwrap();
    let version: i64 = external
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert!(version >= 2);
    let held = host.inner.backend.lock().unwrap();
    host.handle_by(&request, QueryDeadline::after(Duration::from_millis(100)))
        .unwrap();
    assert_eq!(
        external
            .query_row("SELECT COUNT(*) FROM actors", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(held);
}

#[test]
fn ensure_writer_migrates_once_for_router_startup() {
    let directory = crate::board::board_test_support::scratch("board-runtime-");
    let database = directory.path().join("board.sqlite3");
    let host = BoardHost::with_config(BoardConfig::for_database(&database));
    assert!(!database.exists());
    host.ensure_writer(QueryDeadline::start()).unwrap();
    let version: i64 = rusqlite::Connection::open(&database)
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, crate::board::SCHEMA_VERSION);
    assert!(host.inner.backend.lock().unwrap().is_some());
    host.ensure_writer(QueryDeadline::start()).unwrap();
    assert!(host.inner.backend.lock().unwrap().is_some());
}

#[test]
fn ensure_writer_migrates_stale_storage_for_router_startup() {
    let directory = crate::board::board_test_support::scratch("board-runtime-");
    let database = directory.path().join("board.sqlite3");
    crate::board::local_board::seed_storage_schema(&database, crate::board::SCHEMA_VERSION - 1)
        .unwrap();
    let host = BoardHost::with_config(BoardConfig::for_database(&database));
    host.ensure_writer(QueryDeadline::start()).unwrap();
    let version: i64 = rusqlite::Connection::open(&database)
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, crate::board::SCHEMA_VERSION);
    assert!(host.inner.backend.lock().unwrap().is_some());
}
