use super::*;

#[test]
fn startup_refuses_an_erroring_router_and_ingestion_stays_strict() {
    let database = Path::new("/source/web.sqlite3");
    let error = || unavailable("router socket permission denied");
    let failure = startup_probe(
        database,
        QueryDeadline::after(Duration::from_secs(1)),
        Duration::ZERO,
        &mut |_, _| Err(error()),
    )
    .unwrap_err();
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
    startup_probe(
        database,
        QueryDeadline::after(Duration::from_secs(1)),
        Duration::ZERO,
        &mut |_, _| Ok(Some(status.to_string())),
    )
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
    startup_probe(
        &database,
        QueryDeadline::after(Duration::from_secs(1)),
        Duration::ZERO,
        &mut |_, _| Ok(None),
    )
    .unwrap();
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
        let error = startup_probe(
            &database,
            QueryDeadline::after(Duration::from_secs(1)),
            Duration::ZERO,
            &mut |_, _| Ok(None),
        )
        .unwrap_err();
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
    startup_probe(
        &database,
        QueryDeadline::after(Duration::from_secs(1)),
        Duration::ZERO,
        &mut |_, _| Ok(None),
    )
    .unwrap();
}

#[test]
fn startup_without_router_schema_defers_to_the_local_database() {
    let directory = crate::board::board_test_support::scratch("web-probe-");
    let absent = directory.path().join("absent.sqlite3");
    let status = serde_json::json!({
        "status": "ok", "board_api": BOARD_API, "board_db": absent,
    });
    startup_probe(
        &absent,
        QueryDeadline::after(Duration::from_secs(1)),
        Duration::ZERO,
        &mut |_, _| Ok(Some(status.to_string())),
    )
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
    let error = startup_probe(
        &old,
        QueryDeadline::after(Duration::from_secs(1)),
        Duration::ZERO,
        &mut |_, _| Ok(Some(status.to_string())),
    )
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
        let error = startup_probe(
            database,
            QueryDeadline::after(Duration::from_secs(1)),
            Duration::ZERO,
            &mut |_, _| Ok(Some(status.to_string())),
        )
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
    let mut polls = 0;
    startup_probe(
        &database,
        QueryDeadline::after(Duration::from_secs(10)),
        Duration::from_secs(10),
        &mut |_, _| {
            polls += 1;
            Ok(if polls < 3 {
                None
            } else {
                Some(status.clone())
            })
        },
    )
    .unwrap();
    assert_eq!(polls, 3);
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
            "start trufflepig system ensure with the current binary",
        ),
        (
            Some(unmigrated.clone()),
            "router is migrating; check `systemctl --user status trufflepig-system`",
        ),
    ] {
        let mut polls = 0;
        let error = startup_probe(
            &database,
            QueryDeadline::after(Duration::from_secs(1)),
            Duration::from_millis(300),
            &mut |_, _| {
                polls += 1;
                Ok(answered.clone())
            },
        )
        .unwrap_err();
        assert_eq!(error.code, BoardErrorCode::BoardUnavailable);
        assert!(error.to_string().contains(advice), "{error}");
        assert_eq!(polls, 2, "answered: {}", answered.is_some());
    }
    assert_eq!(
        local_schema_version(&database).unwrap(),
        Some(SCHEMA_VERSION - 1),
        "the probe must not migrate"
    );
}
