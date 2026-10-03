//! The browser owns operation inputs; captured local identity owns every request.
use super::WebStore;
use crate::board::{
    board_backend::BoardBackend,
    board_config::BoardConfig,
    board_protocol::{BOARD_API, BoardError, BoardErrorCode, BoardOp, BoardReply, BoardRequest},
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
                    | "show"
                    | "tasks"
                    | "claims"
                    | "feed"
                    | "attention"
                    | "history"
                    | "entries"
                    | "search"
                    | "review"
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
            return Err(invalid("operation is unavailable in the web board"));
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
    let mut reply = if request.op.is_read_only() {
        store
            .readers
            .with_reader(&config, expires, |reader| reader.handle(&request))?
    } else {
        store.with_writer(&config, expires, |writer| writer.handle(&request))?
    };
    if matches!(request.op, BoardOp::Review { .. }) {
        reply.warnings.push(
            "web review contains stored evidence only; use ingest to refresh linked commits".into(),
        );
    }
    Ok(reply)
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

fn startup_probe(
    database: &Path,
    deadline: QueryDeadline,
    exchange: &mut RouterExchange<'_>,
) -> Result<(), BoardError> {
    // Local reads and writes do not depend on a responsive router. Answered identity
    // mismatches remain fatal; ingestion uses the strict probe below.
    probe_router(database, deadline, &mut |args, deadline| {
        Ok(exchange(args, deadline).unwrap_or(None))
    })
    .map(|_| ())
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

fn probe_router(
    database: &Path,
    deadline: QueryDeadline,
    exchange: &mut RouterExchange<'_>,
) -> Result<bool, BoardError> {
    if deadline.expired() {
        return Err(super::deadline_error());
    }
    let Some(reply) = exchange(&["system".into(), "status".into()], deadline)? else {
        return Ok(false);
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
    Ok(true)
}

fn ingest_via(
    database: &Path,
    deadline: QueryDeadline,
    exchange: &mut RouterExchange<'_>,
) -> Result<serde_json::Value, BoardError> {
    if !probe_router(database, deadline, exchange)? {
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
    fn startup_accepts_unreachable_router_but_ingestion_remains_strict() {
        let database = Path::new("/source/web.sqlite3");
        let error = || unavailable("router socket permission denied");
        startup_probe(
            database,
            QueryDeadline::after(Duration::from_secs(1)),
            &mut |_, _| Err(error()),
        )
        .unwrap();
        let failure = ingest_via(
            database,
            QueryDeadline::after(Duration::from_secs(1)),
            &mut |_, _| Err(error()),
        )
        .unwrap_err();
        assert_eq!(failure.code, BoardErrorCode::BoardUnavailable);
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
}
