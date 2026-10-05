//! Ingest route: queue a relay flight and publish its receipt to streams.
use super::super::{
    BoardWebState, http_wire,
    web_ops::{relay_flight, relay_ingest},
};
use super::route_replies::send_error;
use crate::board::board_protocol::{BOARD_API, BoardError, BoardErrorCode};
use serde::Deserialize;
use std::{net::TcpStream, sync::Arc, time::Instant};

/// 202 immediately with the flight ticket; one router scan runs at a time,
/// its completion published to stream subscribers as an `ingest` frame, and
/// mid-scan POSTs take the next ticket and rerun until one scan runs clean.
pub(super) fn ingest_accepted(mut stream: TcpStream, state: &Arc<BoardWebState>) {
    let claim = state.ingest.begin();
    if claim.leads {
        let relay_state = Arc::clone(state);
        let spawned = std::thread::Builder::new()
            .name("board-ingest-relay".into())
            .spawn(move || {
                relay_flight(&relay_state.ingest, || {
                    // Every scan reads its own ticket and restarts the
                    // router reply budget; a rerun never inherits the
                    // first scan's spent deadline.
                    let ticket = relay_state.ingest.current_ticket();
                    let expires = Instant::now() + crate::daemon::CLIENT_REPLY_WAIT;
                    let result = relay_ingest(&relay_state.store, expires);
                    relay_state
                        .streams
                        .publish_ingest(&ingest_receipt(result, &ticket));
                });
            });
        if spawned.is_err() {
            state.ingest.finish();
            send_error(
                &mut stream,
                503,
                "board_unavailable",
                "ingest relay unavailable",
            );
            return;
        }
    }
    let body = serde_json::to_vec(&serde_json::json!({
        "api": BOARD_API, "ingest": "queued", "ticket": claim.ticket,
    }))
    .expect("queued serialization cannot fail");
    let _ = http_wire::send_response(&mut stream, 202, "application/json", &body);
}

/// Relay outcomes keep the board error envelope so subscribers render a
/// terminal state whether the scan succeeded or failed; every receipt carries
/// its flight ticket so tabs complete only on their own scan.
fn ingest_receipt(
    result: Result<serde_json::Value, BoardError>,
    ticket: &str,
) -> serde_json::Value {
    let mut receipt = match result {
        Ok(receipt) => receipt,
        Err(error) => serde_json::json!({
            "error": { "code": error.code.as_str(), "message": error.message }
        }),
    };
    match receipt.as_object_mut() {
        Some(object) => {
            object.insert("ticket".into(), serde_json::json!(ticket));
        }
        None => {
            receipt = serde_json::json!({ "ticket": ticket, "receipt": receipt });
        }
    }
    receipt
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ApiRequest {
    pub(super) api: u32,
}

impl ApiRequest {
    pub(super) fn validate(&self) -> Result<(), BoardError> {
        if self.api != BOARD_API {
            return Err(BoardError::new(
                BoardErrorCode::BoardApiMismatch,
                format!("expected {BOARD_API}, received {}", self.api),
            ));
        }
        Ok(())
    }
}
