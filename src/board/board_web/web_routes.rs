//! Public assets are empty of board data; every board read and write is authenticated.
#[cfg(test)]
mod tests;
use super::{
    PUBLIC_DETAILS, PUBLIC_DOM, PUBLIC_ENTRIES, PUBLIC_FEEDBACK, PUBLIC_READER, PUBLIC_SCRIPT,
    PUBLIC_SHELL, PUBLIC_STREAM, PUBLIC_STYLE, PUBLIC_VIEWS, WebState,
    event_stream::StreamRequest,
    http_wire::{self, HttpError, HttpMethod, HttpRequest},
    plan_markup,
    web_ops::{self, WebRequest},
};
use crate::board::{
    board_ids::{BoardRef, PlanRevision},
    board_protocol::{BOARD_API, BoardError, BoardErrorCode, BoardOp, BoardReply, BoardResult},
    review_packet::build_ssot_diff,
};
use serde::Deserialize;
use std::{net::TcpStream, time::Instant};

pub(super) fn handle(mut stream: TcpStream, accepted_at: Instant, state: &WebState) {
    let request = match http_wire::read_request(&mut stream, accepted_at) {
        Ok(request) => request,
        Err(error) => {
            send_http_error(&mut stream, error);
            return;
        }
    };
    let public = request.path() == "/" || public_asset(request.path()).is_some();
    if let Err(error) = state
        .guard
        .authorize(&request, !public, request.method == HttpMethod::Post)
    {
        send_http_error(&mut stream, error);
        return;
    }
    let expires = accepted_at + http_wire::REQUEST_TIMEOUT;
    if Instant::now() >= expires {
        send_http_error(
            &mut stream,
            HttpError {
                status: 408,
                message: "request deadline expired",
            },
        );
        return;
    }
    if !public {
        if let Err(error) = state.store.config(expires) {
            send_board_error(&mut stream, error);
            return;
        }
    }
    match (request.method, request.path()) {
        (HttpMethod::Get, "/") => {
            let shell = PUBLIC_SHELL.replace("__BOARD_API__", &BOARD_API.to_string());
            let _ = http_wire::send_response(
                &mut stream,
                200,
                "text/html; charset=utf-8",
                shell.as_bytes(),
                false,
            );
        }
        (HttpMethod::Get, path) if public_asset(path).is_some() => {
            let (content_type, body) = public_asset(path).unwrap();
            let _ =
                http_wire::send_response(&mut stream, 200, content_type, body.as_bytes(), false);
        }
        (HttpMethod::Get, "/api/v1/events") => {
            let prepared = stream_request(&request)
                .and_then(|request| state.streams.reserve().map(|permit| (request, permit)));
            match prepared {
                Ok((request, permit)) => {
                    let _ = state.streams.spawn(stream, request, permit);
                }
                Err(error) => send_board_error(&mut stream, error),
            }
        }
        (HttpMethod::Post, "/api/v1/board") => {
            let result = decode::<WebRequest>(&request)
                .and_then(|request| web_ops::execute(&state.store, request, expires));
            send_json_result(&mut stream, result);
        }
        (HttpMethod::Post, "/api/v1/ingest") => {
            let result = decode::<ApiRequest>(&request).and_then(|request| {
                request.validate()?;
                web_ops::relay_ingest(&state.store, expires)
            });
            send_json_result(&mut stream, result);
        }
        (HttpMethod::Get, path) if path.starts_with("/api/v1/render/plan/") => {
            let target = &path["/api/v1/render/plan/".len()..];
            send_json_result(&mut stream, render_plan(state, target, expires));
        }
        (HttpMethod::Get, path) if path.starts_with("/api/v1/render/diff/") => {
            let target = &path["/api/v1/render/diff/".len()..];
            send_json_result(&mut stream, render_diff(state, target, expires));
        }
        (HttpMethod::Get, path) if path.starts_with("/api/v1/render/proposal/") => {
            let target = &path["/api/v1/render/proposal/".len()..];
            send_json_result(&mut stream, render_proposal(state, target, expires));
        }
        _ => send_error(&mut stream, 404, "invalid_reference", "route not found"),
    }
}

fn public_asset(path: &str) -> Option<(&str, &str)> {
    let script = match path {
        "/app.js" => PUBLIC_SCRIPT,
        "/board_dom.js" => PUBLIC_DOM,
        "/board_views.js" => PUBLIC_VIEWS,
        "/board_details.js" => PUBLIC_DETAILS,
        "/board_stream.js" => PUBLIC_STREAM,
        "/board_feedback.js" => PUBLIC_FEEDBACK,
        "/board_reader.js" => PUBLIC_READER,
        "/board_entries.js" => PUBLIC_ENTRIES,
        "/app.css" => return Some(("text/css; charset=utf-8", PUBLIC_STYLE)),
        _ => return None,
    };
    Some(("text/javascript; charset=utf-8", script))
}

fn render_plan(
    state: &WebState,
    target: &str,
    expires: Instant,
) -> Result<serde_json::Value, BoardError> {
    let target = BoardRef::parse(target).map_err(BoardError::from)?;
    if !matches!(target, BoardRef::Plan(_) | BoardRef::Revision(_)) {
        return Err(invalid("render plan requires P# or P#@#"));
    }
    let reply = web_ops::execute(
        &state.store,
        WebRequest {
            api: BOARD_API,
            op: BoardOp::Show { target },
        },
        expires,
    )?;
    let revision = match &reply.result {
        BoardResult::Plan(view) => &view.revision,
        BoardResult::Revision(revision) => revision,
        _ => return Err(invalid("plan rendering returned no revision")),
    };
    let rendered = plan_markup::render(revision.body.as_str());
    let mut value = serde_json::to_value(rendered).map_err(|error| invalid(error.to_string()))?;
    value["api"] = serde_json::json!(BOARD_API);
    value["revision"] = serde_json::json!(revision.id);
    value["snapshot_seq"] = serde_json::json!(reply.snapshot_seq);
    Ok(value)
}

fn render_diff(
    state: &WebState,
    target: &str,
    expires: Instant,
) -> Result<serde_json::Value, BoardError> {
    let target = BoardRef::parse(target).map_err(BoardError::from)?;
    if !matches!(target, BoardRef::Span(span) if span.end.is_some()) {
        return Err(invalid("render diff requires P#@#..#"));
    }
    let reply = web_ops::execute(
        &state.store,
        WebRequest {
            api: BOARD_API,
            op: BoardOp::Show { target },
        },
        expires,
    )?;
    let BoardResult::Diff(diff) = &reply.result else {
        return Err(invalid("diff rendering returned no revisions"));
    };
    let ssot = build_ssot_diff(&diff.before, &diff.after);
    Ok(
        serde_json::json!({ "api": BOARD_API, "before": ssot.before, "after": ssot.after,
        "hunks": ssot.hunks, "snapshot_seq": reply.snapshot_seq }),
    )
}

fn render_proposal(
    state: &WebState,
    target: &str,
    expires: Instant,
) -> Result<serde_json::Value, BoardError> {
    proposal_diff(target, |target| {
        web_ops::execute(
            &state.store,
            WebRequest {
                api: BOARD_API,
                op: BoardOp::Show { target },
            },
            expires,
        )
    })
}

fn proposal_diff(
    target: &str,
    mut read: impl FnMut(BoardRef) -> Result<BoardReply, BoardError>,
) -> Result<serde_json::Value, BoardError> {
    let target = BoardRef::parse(target).map_err(BoardError::from)?;
    if !matches!(target, BoardRef::Entry(_)) {
        return Err(invalid("render proposal requires E#"));
    }
    let entry = read(target)?;
    let BoardResult::Entry(view) = &entry.result else {
        return Err(invalid("proposal rendering returned no entry"));
    };
    let proposal = view
        .proposal
        .as_ref()
        .ok_or_else(|| invalid("entry is not a proposal"))?;
    let base_id =
        PlanRevision::new(proposal.plan, proposal.base_revision).map_err(BoardError::from)?;
    let base = read(BoardRef::Revision(base_id))?;
    let BoardResult::Revision(before) = &base.result else {
        return Err(invalid("proposal base revision is unavailable"));
    };
    let mut proposed = before.clone();
    proposed.body = proposal.body.clone();
    let diff = build_ssot_diff(before, &proposed);
    let snapshot_seq = entry
        .snapshot_seq
        .zip(base.snapshot_seq)
        .map(|(entry, base)| entry.min(base));
    Ok(
        serde_json::json!({ "api": BOARD_API, "entry": proposal.entry, "before": before.id,
        "hunks": diff.hunks, "snapshot_seq": snapshot_seq }),
    )
}

fn stream_request(request: &HttpRequest) -> Result<StreamRequest, BoardError> {
    let (mut after, mut plan) = (None, None);
    if let Some((_, query)) = request.target.split_once('?') {
        for pair in query.split('&') {
            let (key, value) = pair
                .split_once('=')
                .ok_or_else(|| invalid("invalid event query"))?;
            let slot = match key {
                "after" => &mut after,
                "plan" => &mut plan,
                _ => return Err(invalid("unknown event query field")),
            };
            if slot.replace(value).is_some() {
                return Err(invalid("duplicate event query field"));
            }
        }
    }
    StreamRequest::parse(request.header("last-event-id"), after, plan)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiRequest {
    api: u32,
}

impl ApiRequest {
    fn validate(&self) -> Result<(), BoardError> {
        if self.api != BOARD_API {
            return Err(BoardError::new(
                BoardErrorCode::BoardApiMismatch,
                format!("expected {BOARD_API}, received {}", self.api),
            ));
        }
        Ok(())
    }
}

fn decode<T: serde::de::DeserializeOwned>(request: &HttpRequest) -> Result<T, BoardError> {
    serde_json::from_slice(&request.body)
        .map_err(|error| invalid(format!("invalid JSON request: {error}")))
}

fn send_json_result(stream: &mut TcpStream, result: Result<impl serde::Serialize, BoardError>) {
    match result {
        Ok(value) => match serde_json::to_vec(&value) {
            Ok(body) => {
                let _ = http_wire::send_response(stream, 200, "application/json", &body, true);
            }
            Err(error) => send_error(stream, 503, "board_unavailable", &error.to_string()),
        },
        Err(error) => send_board_error(stream, error),
    }
}

fn send_board_error(stream: &mut TcpStream, error: BoardError) {
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

fn send_http_error(stream: &mut TcpStream, error: HttpError) {
    let code = match error.status {
        403 => "forbidden",
        408 => "timed_out",
        413 => "invalid_body",
        _ => "invalid_options",
    };
    send_error(stream, error.status, code, error.message);
}

fn send_error(stream: &mut TcpStream, status: u16, code: &str, message: &str) {
    let body =
        serde_json::to_vec(&serde_json::json!({ "error": { "code": code, "message": message } }))
            .expect("error serialization cannot fail");
    if status == 503 {
        let _ = http_wire::send_unavailable(stream, 1, &body);
    } else {
        let _ = http_wire::send_response(stream, status, "application/json", &body, true);
    }
}

fn invalid(message: impl Into<String>) -> BoardError {
    BoardError::new(BoardErrorCode::InvalidOptions, message)
}
