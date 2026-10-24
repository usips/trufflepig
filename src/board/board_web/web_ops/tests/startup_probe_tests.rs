use super::*;

const STATUS_PROBE_BOUND: Duration = Duration::from_millis(500);

#[test]
fn startup_refuses_an_erroring_router_and_ingestion_stays_strict() {
    let database = Path::new("/source/web.sqlite3");
    let error = || unavailable("router socket permission denied");
    let failure = startup_probe(database, Duration::ZERO, &mut |_, _| Err(error())).unwrap_err();
    assert_eq!(failure.code, BoardErrorCode::BoardUnavailable);
    let failure = ingest_via(
        database,
        QueryDeadline::after(Duration::from_secs(1)),
        &mut |_, _| Err(error()),
    )
    .unwrap_err();
    assert_eq!(failure.code, BoardErrorCode::BoardUnavailable);
}

#[test]
fn startup_accepts_a_same_api_router_reporting_the_current_schema() {
    let database = Path::new("/source/web.sqlite3");
    let status = serde_json::json!({
        "status": "ok", "board_api": BOARD_API, "board_db": database,
        "schema_file": SCHEMA_VERSION, "schema_supported": SCHEMA_VERSION,
    });
    startup_probe(database, Duration::ZERO, &mut |_, _| {
        Ok(Some(status.to_string()))
    })
    .unwrap();
}

#[test]
fn router_confirmation_requires_a_migrated_file_the_binary_supports() {
    let migrated = serde_json::json!({
        "schema_file": SCHEMA_VERSION, "schema_supported": SCHEMA_VERSION,
    });
    assert!(router_confirms_schema(&migrated));
    for status in [
        serde_json::json!({}),
        serde_json::json!({"schema_supported": SCHEMA_VERSION}),
        serde_json::json!({"schema_file": SCHEMA_VERSION}),
        serde_json::json!({
            "schema_file": SCHEMA_VERSION - 1, "schema_supported": SCHEMA_VERSION,
        }),
        serde_json::json!({
            "schema_file": SCHEMA_VERSION + 1, "schema_supported": SCHEMA_VERSION + 1,
        }),
    ] {
        assert!(!router_confirms_schema(&status), "{status}");
    }
}

#[test]
fn startup_bootstraps_an_absent_database_without_a_router() {
    let directory = crate::board::board_test_support::scratch("web-probe-");
    let database = directory.path().join("board.sqlite3");
    let mut polls = 0;
    startup_probe(&database, Duration::ZERO, &mut |_, deadline| {
        polls += 1;
        assert!(!deadline.expired());
        assert!(deadline.remaining() <= STATUS_PROBE_BOUND);
        Ok(None)
    })
    .unwrap();
    assert_eq!(polls, 1, "wait zero still performs one bounded probe");
    assert!(!database.exists(), "the probe must not create the database");
}

#[test]
fn startup_refuses_off_schema_databases_without_a_router() {
    let directory = crate::board::board_test_support::scratch("web-probe-");
    for version in [SCHEMA_VERSION - 1, SCHEMA_VERSION + 1] {
        let database = directory.path().join(format!("v{version}.sqlite3"));
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .pragma_update(None, "user_version", version)
            .unwrap();
        drop(connection);
        let error = startup_probe(&database, Duration::ZERO, &mut |_, _| Ok(None)).unwrap_err();
        assert_eq!(error.code, BoardErrorCode::BoardUnavailable, "v{version}");
        assert_eq!(
            local_schema_version(&database).unwrap(),
            Some(version),
            "the probe must not migrate"
        );
    }
}

#[test]
fn startup_opens_a_current_database_without_a_router() {
    let directory = crate::board::board_test_support::scratch("web-probe-");
    let database = directory.path().join("board.sqlite3");
    let config = BoardConfig::for_database(&database);
    drop(crate::board::local_board::LocalBoard::open(&config).unwrap());
    // The supported schema constant tracks the migrated storage schema.
    assert_eq!(
        local_schema_version(&database).unwrap(),
        Some(SCHEMA_VERSION)
    );
    startup_probe(&database, Duration::ZERO, &mut |_, _| Ok(None)).unwrap();
}

#[test]
fn startup_without_router_schema_defers_to_the_local_database() {
    let directory = crate::board::board_test_support::scratch("web-probe-");
    let absent = directory.path().join("absent.sqlite3");
    let status = serde_json::json!({
        "status": "ok", "board_api": BOARD_API, "board_db": absent,
    });
    startup_probe(&absent, Duration::ZERO, &mut |_, _| {
        Ok(Some(status.to_string()))
    })
    .unwrap();
    let old = directory.path().join("old.sqlite3");
    let connection = rusqlite::Connection::open(&old).unwrap();
    connection
        .pragma_update(None, "user_version", SCHEMA_VERSION - 1)
        .unwrap();
    drop(connection);
    let status = serde_json::json!({
        "status": "ok", "board_api": BOARD_API, "board_db": old,
    });
    let error = startup_probe(&old, Duration::ZERO, &mut |_, _| {
        Ok(Some(status.to_string()))
    })
    .unwrap_err();
    assert_eq!(error.code, BoardErrorCode::BoardUnavailable);
}

#[test]
fn startup_refuses_answered_router_identity_mismatches() {
    let database = Path::new("/source/web.sqlite3");
    for status in [
        serde_json::json!({"status":"ok","board_api":BOARD_API - 1,"board_db":database}),
        serde_json::json!({"status":"ok","board_api":BOARD_API,"board_db":"/source/other.sqlite3"}),
    ] {
        let error = startup_probe(database, Duration::ZERO, &mut |_, _| {
            Ok(Some(status.to_string()))
        })
        .unwrap_err();
        assert_eq!(error.code, BoardErrorCode::BoardUnavailable);
    }
}

#[test]
fn startup_waits_for_a_migrating_router_before_opening() {
    let directory = crate::board::board_test_support::scratch("web-probe-");
    let database = directory.path().join("migrating.sqlite3");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .pragma_update(None, "user_version", SCHEMA_VERSION - 1)
        .unwrap();
    drop(connection);
    let status = serde_json::json!({
        "status": "ok", "board_api": BOARD_API, "board_db": database,
        "schema_file": SCHEMA_VERSION, "schema_supported": SCHEMA_VERSION,
    })
    .to_string();
    let started = std::time::Instant::now();
    let mut polls = 0;
    startup_probe(&database, Duration::from_millis(700), &mut |_, deadline| {
        polls += 1;
        let remaining = deadline.remaining();
        assert!(
            !remaining.is_zero(),
            "poll {polls} must be bounded and live"
        );
        assert!(remaining <= STATUS_PROBE_BOUND, "poll {polls}");
        match polls {
            1 => Ok(None),
            2 => {
                assert!(
                    started.elapsed() >= Duration::from_millis(450),
                    "the next status poll follows the migration poll interval"
                );
                assert!(
                    remaining <= Duration::from_millis(250),
                    "the second poll shares the original 700 ms migration deadline"
                );
                Ok(Some(status.clone()))
            }
            _ => panic!("unexpected migration poll {polls}"),
        }
    })
    .unwrap();
    assert_eq!(polls, 2);
}

#[test]
fn startup_migration_outlives_ordinary_web_request_timeout() {
    let directory = crate::board::board_test_support::scratch("web-probe-");
    let database = directory.path().join("migrating.sqlite3");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .pragma_update(None, "user_version", SCHEMA_VERSION - 1)
        .unwrap();
    drop(connection);
    let status = serde_json::json!({
        "status": "ok", "board_api": BOARD_API, "board_db": database,
        "schema_file": SCHEMA_VERSION, "schema_supported": SCHEMA_VERSION,
    })
    .to_string();
    let ordinary_request_timeout = crate::board::board_web::http_wire::REQUEST_TIMEOUT;
    let status_available_after = ordinary_request_timeout + Duration::from_millis(100);
    let started = std::time::Instant::now();

    startup_probe(&database, ROUTER_MIGRATION_WAIT, &mut |_, deadline| {
        let remaining = deadline.remaining();
        assert!(!remaining.is_zero());
        assert!(remaining <= STATUS_PROBE_BOUND);
        if started.elapsed() <= status_available_after {
            return Ok(None);
        }
        Ok(Some(status.clone()))
    })
    .unwrap();

    assert!(started.elapsed() > status_available_after);
    assert!(started.elapsed() < ROUTER_MIGRATION_WAIT);
}

#[test]
fn startup_refuses_a_stale_file_after_the_migration_wait() {
    let directory = crate::board::board_test_support::scratch("web-probe-");
    let database = directory.path().join("stale.sqlite3");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .pragma_update(None, "user_version", SCHEMA_VERSION - 1)
        .unwrap();
    drop(connection);
    let unmigrated = serde_json::json!({
        "status": "ok", "board_api": BOARD_API, "board_db": database,
        "schema_file": SCHEMA_VERSION - 1, "schema_supported": SCHEMA_VERSION,
    })
    .to_string();
    for (answered, advice) in [
        (
            None,
            "run `trufflepig system ensure` with the current binary",
        ),
        (
            Some(unmigrated.clone()),
            "router is migrating; check `systemctl --user status trufflepig-system`",
        ),
    ] {
        let mut polls = 0;
        let error = startup_probe(&database, Duration::from_millis(300), &mut |_, deadline| {
            polls += 1;
            assert!(!deadline.expired());
            assert!(deadline.remaining() <= STATUS_PROBE_BOUND);
            Ok(answered.clone())
        })
        .unwrap_err();
        assert_eq!(error.code, BoardErrorCode::BoardUnavailable);
        assert!(error.to_string().contains(advice), "{error}");
        assert_eq!(polls, 1, "answered: {}", answered.is_some());
    }
    assert_eq!(
        local_schema_version(&database).unwrap(),
        Some(SCHEMA_VERSION - 1),
        "the probe must not migrate"
    );
}

#[test]
fn startup_polls_through_router_reply_timeouts_while_it_migrates() {
    let directory = crate::board::board_test_support::scratch("web-probe-");
    let database = directory.path().join("busy.sqlite3");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .pragma_update(None, "user_version", SCHEMA_VERSION - 1)
        .unwrap();
    drop(connection);
    let status = serde_json::json!({
        "status": "ok", "board_api": BOARD_API, "board_db": database,
        "schema_file": SCHEMA_VERSION, "schema_supported": SCHEMA_VERSION,
    })
    .to_string();
    let started = std::time::Instant::now();
    let mut polls = 0;
    startup_probe(&database, Duration::from_secs(5), &mut |_, deadline| {
        polls += 1;
        assert!(deadline.remaining() <= STATUS_PROBE_BOUND, "poll {polls}");
        if polls <= 2 {
            return Err(router_reply_timeout());
        }
        Ok(Some(status.clone()))
    })
    .unwrap();
    assert_eq!(polls, 3);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn startup_reports_a_busy_router_past_the_migration_wait_as_migrating() {
    let directory = crate::board::board_test_support::scratch("web-probe-");
    let database = directory.path().join("stuck.sqlite3");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .pragma_update(None, "user_version", SCHEMA_VERSION - 1)
        .unwrap();
    drop(connection);
    let mut polls = 0;
    let error = startup_probe(&database, Duration::from_millis(700), &mut |_, deadline| {
        polls += 1;
        assert!(!deadline.expired());
        assert!(deadline.remaining() <= STATUS_PROBE_BOUND);
        Err(router_reply_timeout())
    })
    .unwrap_err();
    assert_eq!(error.code, BoardErrorCode::BoardUnavailable);
    assert!(
        error
            .message
            .contains("router is migrating; check `systemctl --user status trufflepig-system`"),
        "{error}"
    );
    assert!(
        polls >= 2,
        "a busy router is polled until the wait ends: {polls}"
    );
    assert_eq!(
        local_schema_version(&database).unwrap(),
        Some(SCHEMA_VERSION - 1),
        "the probe must not migrate"
    );
}

#[test]
fn startup_keeps_other_router_failures_fatal_on_the_first_probe() {
    let directory = crate::board::board_test_support::scratch("web-probe-");
    let database = directory.path().join("refused.sqlite3");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .pragma_update(None, "user_version", SCHEMA_VERSION - 1)
        .unwrap();
    drop(connection);
    for error in [
        unavailable("router socket permission denied"),
        BoardError::from(anyhow::anyhow!(coded!(TimedOut, "phase=connecting"))),
    ] {
        let mut polls = 0;
        let failure = startup_probe(&database, Duration::from_millis(700), &mut |_, _| {
            polls += 1;
            Err(error.clone())
        })
        .unwrap_err();
        assert_eq!(failure, error);
        assert_eq!(polls, 1, "{error}");
    }
}

fn router_reply_timeout() -> BoardError {
    BoardError::from(anyhow::Error::new(crate::daemon::DaemonReplyTimeout::new(
        Duration::from_millis(500),
        Duration::from_millis(500),
    )))
}
