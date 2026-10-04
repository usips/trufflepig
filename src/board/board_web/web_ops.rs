//! The browser owns operation inputs; captured local identity owns every request.
use super::WebStore;
use crate::board::{
    board_backend::BoardBackend,
    board_config::BoardConfig,
    board_protocol::{
        BOARD_API, BOARD_SCHEMA_VERSION, BoardError, BoardErrorCode, BoardOp, BoardReply,
        BoardRequest,
    },
    board_vocabulary::EntryKind,
};
use crate::daemon::deadline::QueryDeadline;
use serde::Deserialize;
use std::{path::Path, time::Instant};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WebRequest {
    pub api: u32,
    pub op: BoardOp,
}

impl WebRequest {
    pub(crate) fn into_request(self, config: &BoardConfig) -> Result<BoardRequest, BoardError> {
        if self.api != BOARD_API {
            return Err(BoardError::new(
                BoardErrorCode::BoardApiMismatch,
                format!("expected {BOARD_API}, received {}", self.api),
            ));
        }
        let name = serde_json::to_value(&self.op).map_err(|error| invalid(error.to_string()))?;
        let allowed = matches!(
            name["op"].as_str(),
            Some(
                "overview"
                    | "repositories"
                    | "show"
                    | "tasks"
                    | "claims"
                    | "feed"
                    | "attention"
                    | "history"
                    | "entries"
                    | "search"
                    | "feedback_list"
                    | "new"
                    | "post"
                    | "task_create"
                    | "task_move"
                    | "accept"
                    | "reject"
                    | "edit"
                    | "feedback_close"
                    | "feedback_triage"
            )
        );
        if !allowed {
            return Err(invalid("op not available over web"));
        }
        if let BoardOp::Post { kind, .. } = &self.op {
            if !matches!(
                kind,
                EntryKind::Note | EntryKind::Answer | EntryKind::Decision | EntryKind::Question
            ) {
                return Err(invalid(
                    "the web board accepts note, answer, decision, and question posts",
                ));
            }
        }
        let actor = config
            .actor(Some("human"), Some("web"))
            .map_err(BoardError::from)?;
        let request = BoardRequest::new(actor, self.op);
        request.validate().map_err(BoardError::from)?;
        Ok(request)
    }
}

pub(crate) fn execute(
    store: &WebStore,
    request: WebRequest,
    expires: Instant,
) -> Result<BoardReply, BoardError> {
    let config = store.config(expires)?;
    let request = request.into_request(&config)?;
    let reply = if request.op.is_read_only() {
        store
            .readers
            .with_reader(&config, expires, |reader| reader.handle(&request))?
    } else {
        store.with_writer(&config, expires, |writer| writer.handle(&request))?
    };
    Ok(reply)
}

/// One router ingest relay runs at a time; later posts observe the running scan.
#[derive(Default)]
pub(crate) struct IngestFlight(std::sync::atomic::AtomicBool);

impl IngestFlight {
    /// Claims the relay slot; false while another relay runs.
    pub(crate) fn begin(&self) -> bool {
        !self.0.swap(true, std::sync::atomic::Ordering::AcqRel)
    }

    pub(crate) fn finish(&self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Returns the relay slot on drop, including panic paths.
pub(crate) struct IngestFlightGuard<'a>(&'a IngestFlight);

impl<'a> IngestFlightGuard<'a> {
    pub(crate) fn new(flight: &'a IngestFlight) -> Self {
        Self(flight)
    }
}

impl Drop for IngestFlightGuard<'_> {
    fn drop(&mut self) {
        self.0.finish();
    }
}

fn invalid(message: impl Into<String>) -> BoardError {
    BoardError::new(BoardErrorCode::InvalidOptions, message)
}

type RouterExchange<'a> =
    dyn FnMut(&[String], QueryDeadline) -> Result<Option<String>, BoardError> + 'a;

pub(super) fn check_router_identity(
    runtime: &Path,
    database: &Path,
    expires: Instant,
) -> Result<(), BoardError> {
    let context = crate::diagnostics::RequestContext::new(Some("web".into()), Some("human".into()));
    let deadline = QueryDeadline::after(expires.saturating_duration_since(Instant::now()));
    startup_probe(database, deadline, &mut |args, deadline| {
        crate::daemon::request_by(runtime, args, &context, deadline).map_err(BoardError::from)
    })
}

/// Startup opens the database only when a same-API router confirms its schema,
/// the file already holds the supported schema, or a brand-new database may
/// bootstrap. An answered router error is fatal; only a silent router (`None`)
/// permits the local schema check.
fn startup_probe(
    database: &Path,
    deadline: QueryDeadline,
    exchange: &mut RouterExchange<'_>,
) -> Result<(), BoardError> {
    let confirmed = probe_router(database, deadline, exchange)?
        .and_then(|status| status["schema_version"].as_i64())
        == Some(BOARD_SCHEMA_VERSION);
    if confirmed {
        return Ok(());
    }
    match local_schema_version(database)? {
        None => Ok(()),
        Some(version) if version == BOARD_SCHEMA_VERSION => Ok(()),
        Some(version) if version < BOARD_SCHEMA_VERSION => Err(unavailable(format!(
            concat!(
                "board database schema version {version} awaits migration to {BOARD_SCHEMA_VERSION}; ",
                "start trufflepig system ensure with the current binary"
            ),
            version = version,
            BOARD_SCHEMA_VERSION = BOARD_SCHEMA_VERSION
        ))),
        Some(version) => Err(unavailable(format!(
            "board database schema version {version} is newer than supported {BOARD_SCHEMA_VERSION}"
        ))),
    }
}

/// Reads the storage schema without creating or migrating; `None` means absent.
fn local_schema_version(database: &Path) -> Result<Option<i64>, BoardError> {
    match database.symlink_metadata() {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(unavailable(format!("board database: {error}"))),
    }
    let connection = rusqlite::Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|error| unavailable(format!("board database schema is unreadable: {error}")))?;
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map(Some)
        .map_err(|error| unavailable(format!("board database schema is unreadable: {error}")))
}

pub(super) fn relay_ingest(
    store: &WebStore,
    expires: Instant,
) -> Result<serde_json::Value, BoardError> {
    let config = store.config(expires)?;
    let context = crate::diagnostics::RequestContext::new(Some("web".into()), Some("human".into()));
    let deadline = QueryDeadline::after(expires.saturating_duration_since(Instant::now()));
    ingest_via(&config.db_path, deadline, &mut |args, deadline| {
        crate::daemon::request_by(&store.runtime, args, &context, deadline)
            .map_err(BoardError::from)
    })
}

/// Fatal when a reachable router disagrees on API or database; `None` only
/// means no router answered. The returned status lets callers compare schema.
fn probe_router(
    database: &Path,
    deadline: QueryDeadline,
    exchange: &mut RouterExchange<'_>,
) -> Result<Option<serde_json::Value>, BoardError> {
    if deadline.expired() {
        return Err(super::deadline_error());
    }
    let Some(reply) = exchange(&["system".into(), "status".into()], deadline)? else {
        return Ok(None);
    };
    let status: serde_json::Value =
        serde_json::from_str(&reply).map_err(|_| unavailable("router returned invalid status"))?;
    if status["status"].as_str() != Some("ok")
        || status["board_api"].as_u64() != Some(u64::from(BOARD_API))
    {
        return Err(unavailable(
            "router board_api mismatch; restart trufflepig-system.service",
        ));
    }
    let reported = status["board_db"]
        .as_str()
        .map(Path::new)
        .ok_or_else(|| unavailable("router status omits board_db"))?;
    let same = database == reported
        || database
            .canonicalize()
            .ok()
            .zip(reported.canonicalize().ok())
            .is_some_and(|(local, router)| local == router);
    if !reported.is_absolute() || !same {
        return Err(unavailable(
            "web database differs from router board_db; restore the router database configuration",
        ));
    }
    Ok(Some(status))
}

fn ingest_via(
    database: &Path,
    deadline: QueryDeadline,
    exchange: &mut RouterExchange<'_>,
) -> Result<serde_json::Value, BoardError> {
    // Ingestion runs on the router, which owns migration; schema is not consulted.
    if probe_router(database, deadline, exchange)?.is_none() {
        return Err(unavailable(
            "router is unavailable; start trufflepig system ensure",
        ));
    }
    if deadline.expired() {
        return Err(super::deadline_error());
    }
    let args = ["--json".into(), "board".into(), "ingest".into()];
    let reply = exchange(&args, deadline)?
        .ok_or_else(|| unavailable("router disappeared before ingestion"))?;
    serde_json::from_str(&reply)
        .map_err(|_| unavailable("router returned invalid ingestion receipt"))
}

fn unavailable(message: impl Into<String>) -> BoardError {
    BoardError::new(BoardErrorCode::BoardUnavailable, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn startup_refuses_an_erroring_router_and_ingestion_stays_strict() {
        let database = Path::new("/source/web.sqlite3");
        let error = || unavailable("router socket permission denied");
        let failure = startup_probe(
            database,
            QueryDeadline::after(Duration::from_secs(1)),
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
            "schema_version": BOARD_SCHEMA_VERSION,
        });
        startup_probe(
            database,
            QueryDeadline::after(Duration::from_secs(1)),
            &mut |_, _| Ok(Some(status.to_string())),
        )
        .unwrap();
    }

    #[test]
    fn startup_bootstraps_an_absent_database_without_a_router() {
        let directory = crate::board::board_test_support::scratch("web-probe-");
        let database = directory.path().join("board.sqlite3");
        startup_probe(
            &database,
            QueryDeadline::after(Duration::from_secs(1)),
            &mut |_, _| Ok(None),
        )
        .unwrap();
        assert!(!database.exists(), "the probe must not create the database");
    }

    #[test]
    fn startup_refuses_off_schema_databases_without_a_router() {
        let directory = crate::board::board_test_support::scratch("web-probe-");
        for version in [BOARD_SCHEMA_VERSION - 1, BOARD_SCHEMA_VERSION + 1] {
            let database = directory.path().join(format!("v{version}.sqlite3"));
            let connection = rusqlite::Connection::open(&database).unwrap();
            connection
                .pragma_update(None, "user_version", version)
                .unwrap();
            drop(connection);
            let error = startup_probe(
                &database,
                QueryDeadline::after(Duration::from_secs(1)),
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
            Some(BOARD_SCHEMA_VERSION)
        );
        startup_probe(
            &database,
            QueryDeadline::after(Duration::from_secs(1)),
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
            &mut |_, _| Ok(Some(status.to_string())),
        )
        .unwrap();
        let old = directory.path().join("old.sqlite3");
        let connection = rusqlite::Connection::open(&old).unwrap();
        connection
            .pragma_update(None, "user_version", BOARD_SCHEMA_VERSION - 1)
            .unwrap();
        drop(connection);
        let status = serde_json::json!({
            "status": "ok", "board_api": BOARD_API, "board_db": old,
        });
        let error = startup_probe(
            &old,
            QueryDeadline::after(Duration::from_secs(1)),
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
                &mut |_, _| Ok(Some(status.to_string())),
            )
            .unwrap_err();
            assert_eq!(error.code, BoardErrorCode::BoardUnavailable);
        }
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
    fn ingest_flight_admits_one_relay_at_a_time() {
        let flight = IngestFlight::default();
        assert!(flight.begin());
        assert!(!flight.begin(), "a running scan admits no second relay");
        flight.finish();
        assert!(flight.begin());
    }
}
