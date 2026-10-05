//! Reply writers: JSON results and board/HTTP error envelopes.
use super::super::http_wire::{self, HttpError};
use crate::board::board_protocol::{BoardError, BoardErrorCode};
use std::net::TcpStream;

pub(super) fn send_json_result(
    stream: &mut TcpStream,
    result: Result<impl serde::Serialize, BoardError>,
) {
    match result {
        Ok(value) => match serde_json::to_vec(&value) {
            Ok(body) => {
                let _ = http_wire::send_response(stream, 200, "application/json", &body);
            }
            Err(error) => send_error(stream, 503, "board_unavailable", &error.to_string()),
        },
        Err(error) => send_board_error(stream, error),
    }
}

pub(super) fn send_board_error(stream: &mut TcpStream, error: BoardError) {
    let status = match error.code {
        BoardErrorCode::StaleRevision | BoardErrorCode::ClaimConflict => 409,
        BoardErrorCode::BoardUnavailable
        | BoardErrorCode::DatabaseLocked
        | BoardErrorCode::DaemonBusy
        | BoardErrorCode::BoardRemoteUnsupported => 503,
        _ => 400,
    };
    send_error(stream, status, error.code.as_str(), &error.message);
}

pub(super) fn send_http_error(stream: &mut TcpStream, error: HttpError) {
    let code = match error.status {
        403 => "forbidden",
        408 => "timed_out",
        413 => "invalid_body",
        421 => "misdirected_request",
        _ => "invalid_options",
    };
    if error.status == 405 {
        let body = serde_json::to_vec(
            &serde_json::json!({ "error": { "code": code, "message": error.message } }),
        )
        .expect("error serialization cannot fail");
        let _ = http_wire::send_method_refusal(stream, &body, error.allow.unwrap_or("GET, POST"));
        return;
    }
    send_error(stream, error.status, code, error.message);
}

pub(super) fn send_error(stream: &mut TcpStream, status: u16, code: &str, message: &str) {
    let body =
        serde_json::to_vec(&serde_json::json!({ "error": { "code": code, "message": message } }))
            .expect("error serialization cannot fail");
    if status == 503 {
        let _ = http_wire::send_unavailable(stream, 1, &body);
    } else {
        let _ = http_wire::send_response(stream, status, "application/json", &body);
    }
}
