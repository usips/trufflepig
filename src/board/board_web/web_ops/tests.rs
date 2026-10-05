use super::router_probe::{local_schema_version, router_confirms_schema, unavailable};
use super::*;
use crate::board::{
    SCHEMA_VERSION,
    board_config::BoardConfig,
    board_protocol::{BOARD_API, BoardErrorCode, BoardOp},
};
use std::time::Duration;

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

#[test]
fn web_request_rejects_client_identity_and_agent_claims() {
    for field in ["actor", "claims", "model", "effort"] {
        let mut input =
            serde_json::json!({ "api": BOARD_API, "op": { "op": "show", "target": "P1" } });
        input[field] = serde_json::json!({});
        assert!(serde_json::from_value::<WebRequest>(input).is_err());
    }
}

#[test]
fn web_identity_is_local_human_and_unspoofable() {
    let config = BoardConfig::for_database("target/web-op.sqlite3");
    let request = WebRequest {
        api: BOARD_API,
        op: BoardOp::Show {
            target: crate::board::board_ids::BoardRef::parse("P1").unwrap(),
        },
    }
    .into_request(&config)
    .unwrap();
    assert_eq!(request.actor.identity(), "test@localhost/human/web");
    assert!(request.claims.is_none());
    let rejected = WebRequest {
        api: BOARD_API,
        op: BoardOp::Hello {
            model: "forged".into(),
            effort: None,
        },
    };
    assert!(rejected.into_request(&config).is_err());
}

#[test]
fn ingestion_rejects_old_api_and_another_database_before_forwarding() {
    let database = Path::new("/source/web.sqlite3");
    for status in [
        serde_json::json!({"status":"ok"}),
        serde_json::json!({"status":"ok","board_api":BOARD_API - 1,"board_db":database}),
        serde_json::json!({"status":"ok","board_api":BOARD_API,"board_db":"/source/other.sqlite3"}),
    ] {
        let mut calls = Vec::new();
        let error = ingest_via(
            database,
            QueryDeadline::after(Duration::from_secs(1)),
            &mut |args, _| {
                calls.push(args.to_vec());
                Ok(Some(status.to_string()))
            },
        )
        .unwrap_err();
        assert_eq!(error.code, BoardErrorCode::BoardUnavailable);
        assert_eq!(
            calls,
            vec![vec![String::from("system"), String::from("status")]]
        );
    }
}

#[test]
fn ingestion_probes_then_relays_with_the_same_deadline() {
    let database = Path::new("/source/web.sqlite3");
    let mut calls = Vec::new();
    let deadline = QueryDeadline::after(Duration::from_secs(1));
    let expected_remaining = deadline.remaining();
    let reply = ingest_via(database, deadline, &mut |args, passed| {
        assert!(passed.remaining() <= expected_remaining);
        calls.push(args.to_vec());
        Ok(Some(if calls.len() == 1 {
            serde_json::json!({"status":"ok","board_api":BOARD_API,"board_db":database})
                .to_string()
        } else {
            serde_json::json!({"api":BOARD_API,"inserted":1}).to_string()
        }))
    })
    .unwrap();
    assert_eq!(reply["inserted"], 1);
    assert_eq!(calls[1], ["--json", "board", "ingest"]);
}

#[test]
fn review_is_refused_over_the_web_allowlist() {
    let config = BoardConfig::for_database("target/web-op.sqlite3");
    let request = WebRequest {
        api: BOARD_API,
        op: BoardOp::Review {
            base: crate::board::board_ids::PlanRevision::new(
                crate::board::board_ids::PlanId::new(1).unwrap(),
                1,
            )
            .unwrap(),
            agent: None,
        },
    };
    let error = request.into_request(&config).unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidOptions);
    assert_eq!(error.message, "op not available over web");
}

#[test]
fn link_commit_is_refused_over_the_web_allowlist() {
    let config = BoardConfig::for_database("target/web-op.sqlite3");
    let request = WebRequest {
        api: BOARD_API,
        op: BoardOp::LinkCommit {
            oid: crate::identity::GitOid::parse(&"a".repeat(40)).unwrap(),
            task: crate::board::board_ids::TaskId::new(
                crate::board::board_ids::PlanId::new(1).unwrap(),
                1,
            )
            .unwrap(),
            resolution: None,
        },
    };
    let error = request.into_request(&config).unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidOptions);
    assert_eq!(error.message, "op not available over web");
}

#[test]
fn ingest_flight_admits_one_relay_at_a_time() {
    let flight = IngestFlight::default();
    assert!(flight.begin().leads);
    assert!(
        !flight.begin().leads,
        "a running scan admits no second relay"
    );
    flight.finish();
    assert!(flight.begin().leads);
}

#[test]
fn mid_scan_posts_join_the_ticket_and_rerun_until_clean() {
    let flight = IngestFlight::default();
    let claim = flight.begin();
    assert!(claim.leads);
    let mut scans = 0;
    relay_flight(&flight, || {
        scans += 1;
        // POSTs landing mid-scan join the running flight's ticket and
        // dirty it; concurrent joins coalesce into a single rerun, and
        // the relay stops once a scan runs clean.
        if scans < 3 {
            for _ in 0..2 {
                let joined = flight.begin();
                assert!(!joined.leads);
                assert_eq!(joined.ticket, claim.ticket);
            }
        }
    });
    assert_eq!(scans, 3, "two dirty scans rerun, the clean scan ends it");
    assert!(!flight.take_dirty());
    let next = flight.begin();
    assert!(next.leads);
    assert_ne!(next.ticket, claim.ticket);
}
