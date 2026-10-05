//! Public assets are empty of board data; every board read and write is authenticated.
#[cfg(test)]
mod tests;
use super::{
    PUBLIC_DOM, PUBLIC_ENTRIES, PUBLIC_INGEST, PUBLIC_LRU, PUBLIC_MAIN, PUBLIC_PAGES, PUBLIC_READER,
    PUBLIC_SEEN, PUBLIC_SHELL, PUBLIC_STREAM, PUBLIC_STYLE, PUBLIC_TOKEN, PUBLIC_TRIAGE,
    PUBLIC_VIEWS, WebState,
    event_stream::{EventStreams, StreamPermit, StreamRequest},
    http_wire::{self, HttpError, HttpMethod, HttpRequest},
    plan_markup,
    web_guard::{ChallengeNonce, RouteAccess, WebGuard},
    web_ops::{self, WebRequest},
};
use crate::board::{
    board_ids::{BoardRef, PlanRevision},
    board_protocol::{BOARD_API, BoardError, BoardErrorCode, BoardOp, BoardReply, BoardResult},
    review_packet::build_ssot_diff,
};
use serde::Deserialize;
use std::{net::TcpStream, sync::Arc, time::Instant};

pub(super) fn handle(mut stream: TcpStream, accepted_at: Instant, state: &Arc<WebState>) {
    let request = match http_wire::read_request(&mut stream, accepted_at) {
        Ok(request) => request,
        Err(error) => {
            send_http_error(&mut stream, error);
            http_wire::close_after_error(&mut stream);
            return;
        }
    };
    let access = if request.path() == "/" || public_asset(request.path()).is_some() {
        RouteAccess::Public
    } else if request.path() == "/api/v1/challenge" {
        RouteAccess::Challenge
    } else {
        RouteAccess::Private
    };
    if let Err(error) = state
        .guard
        .authorize(&request, access, request.method == HttpMethod::Post)
    {
        send_http_error(&mut stream, error);
        return;
    }
    let expires = accepted_at + http_wire::REQUEST_TIMEOUT;
    if Instant::now() >= expires {
        send_http_error(&mut stream, HttpError::new(408, "request deadline expired"));
        return;
    }
    if access == RouteAccess::Private {
        if let Err(error) = state.store.config(expires) {
            send_board_error(&mut stream, error);
            return;
        }
    }
    match (request.method, request.path()) {
        (HttpMethod::Get, "/") => {
            let shell = PUBLIC_SHELL
                .replace("__BOARD_API__", &BOARD_API.to_string())
                .replace("__BOARD_ID__", &state.board_id);
            let _ = http_wire::send_response(
                &mut stream,
                200,
                "text/html; charset=utf-8",
                shell.as_bytes(),
                true,
            );
        }
        (HttpMethod::Get, path) if public_asset(path).is_some() => {
            let (content_type, body) = public_asset(path).unwrap();
            let _ = http_wire::send_response(&mut stream, 200, content_type, body.as_bytes(), true);
        }
        (HttpMethod::Post, "/api/v1/challenge") => {
            send_json_result(&mut stream, challenge_reply(&request, &state.guard));
        }
        (HttpMethod::Get, "/api/v1/events") => {
            let prepared = stream_request(&request)
                .and_then(|request| state.streams.reserve().map(|permit| (request, permit)));
            stream_subscription(stream, prepared, &state.streams);
        }
        (HttpMethod::Post, "/api/v1/board") => {
            let result = decode::<WebRequest>(&request)
                .and_then(|request| web_ops::execute(&state.store, request, expires));
            send_json_result(&mut stream, result);
        }
        (HttpMethod::Post, "/api/v1/ingest") => {
            let result = decode::<ApiRequest>(&request).and_then(|request| request.validate());
            match result {
                Ok(()) => ingest_accepted(stream, state),
                Err(error) => send_board_error(&mut stream, error),
            }
        }
        (HttpMethod::Get, path) if path.starts_with("/api/v1/render/plan/") => {
            let target = decode_render_target(&path["/api/v1/render/plan/".len()..]);
            send_json_result(
                &mut stream,
                target.and_then(|target| render_plan(state, &target, expires)),
            );
        }
        (HttpMethod::Get, path) if path.starts_with("/api/v1/render/diff/") => {
            let target = decode_render_target(&path["/api/v1/render/diff/".len()..]);
            send_json_result(
                &mut stream,
                target.and_then(|target| render_diff(state, &target, expires)),
            );
        }
        (HttpMethod::Get, path) if path.starts_with("/api/v1/render/proposal/") => {
            let target = decode_render_target(&path["/api/v1/render/proposal/".len()..]);
            send_json_result(
                &mut stream,
                target.and_then(|target| render_proposal(state, &target, expires)),
            );
        }
        (HttpMethod::Post, "/api/v1/events") => send_http_error(
            &mut stream,
            HttpError::new(405, "events route requires GET").with_allow("GET"),
        ),
        (HttpMethod::Get, "/api/v1/board") => send_http_error(
            &mut stream,
            HttpError::new(405, "board route requires POST").with_allow("POST"),
        ),
        (HttpMethod::Get, "/api/v1/ingest") => send_http_error(
            &mut stream,
            HttpError::new(405, "ingest route requires POST").with_allow("POST"),
        ),
        (HttpMethod::Post, path)
            if path.starts_with("/api/v1/render/plan/")
                || path.starts_with("/api/v1/render/diff/")
                || path.starts_with("/api/v1/render/proposal/") =>
        {
            send_http_error(
                &mut stream,
                HttpError::new(405, "render route requires GET").with_allow("GET"),
            );
        }
        _ => send_error(&mut stream, 404, "invalid_reference", "route not found"),
    }
}

/// A refused spawn (capacity reached, thread spawn failed) still answers the
/// subscriber: reserve failures map to 503 with Retry-After; a failed handoff
/// answers 503 on the returned socket instead of dropping it silently.
fn stream_subscription(
    mut stream: TcpStream,
    prepared: Result<(StreamRequest, StreamPermit), BoardError>,
    streams: &EventStreams,
) {
    match prepared {
        Ok((request, permit)) => {
            if let Err(refusal) = streams.spawn(stream, request, permit) {
                let mut socket = refusal.socket;
                send_error(
                    &mut socket,
                    503,
                    "board_unavailable",
                    &format!("event stream spawn failed: {}", refusal.error),
                );
            }
        }
        Err(error) => send_board_error(&mut stream, error),
    }
}

/// 202 immediately with the flight ticket; one router scan runs at a time,
/// its completion published to stream subscribers as an `ingest` frame, and
/// mid-scan POSTs rerun the scan until one runs clean.
fn ingest_accepted(mut stream: TcpStream, state: &Arc<WebState>) {
    let claim = state.ingest.begin();
    if claim.leads {
        let relay_state = Arc::clone(state);
        let ticket = claim.ticket.clone();
        let spawned = std::thread::Builder::new()
            .name("board-ingest-relay".into())
            .spawn(move || {
                let expires = Instant::now() + crate::daemon::CLIENT_REPLY_WAIT;
                web_ops::relay_flight(&relay_state.ingest, || {
                    let result = web_ops::relay_ingest(&relay_state.store, expires);
                    relay_state
                        .streams
                        .publish_ingest(&ingest_receipt(result, &ticket));
                });
            });
        if spawned.is_err() {
            state.ingest.finish();
            send_error(&mut stream, 503, "board_unavailable", "ingest relay unavailable");
            return;
        }
    }
    let body = serde_json::to_vec(&serde_json::json!({
        "api": BOARD_API, "ingest": "queued", "ticket": claim.ticket,
    }))
    .expect("queued serialization cannot fail");
    let _ = http_wire::send_response(&mut stream, 202, "application/json", &body, true);
}

/// Relay outcomes keep the board error envelope so subscribers render a
/// terminal state whether the scan succeeded or failed; every receipt carries
/// its flight ticket so tabs complete only on their own scan.
fn ingest_receipt(result: Result<serde_json::Value, BoardError>, ticket: &str) -> serde_json::Value {
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

fn public_asset(path: &str) -> Option<(&str, &str)> {
    let script = match path {
        "/board_web_main.js" => PUBLIC_MAIN,
        "/board_dom.js" => PUBLIC_DOM,
        "/board_views.js" => PUBLIC_VIEWS,
        "/board_pages.js" => PUBLIC_PAGES,
        "/board_stream.js" => PUBLIC_STREAM,
        "/feedback_triage.js" => PUBLIC_TRIAGE,
        "/board_reader.js" => PUBLIC_READER,
        "/board_entries.js" => PUBLIC_ENTRIES,
        "/board_web_token.js" => PUBLIC_TOKEN,
        "/board_ingest.js" => PUBLIC_INGEST,
        "/board_lru.js" => PUBLIC_LRU,
        "/board_seen.js" => PUBLIC_SEEN,
        "/board_web.css" => return Some(("text/css; charset=utf-8", PUBLIC_STYLE)),
        _ => return None,
    };
    Some(("text/javascript; charset=utf-8", script))
}

/// Percent-decode a render-route suffix: only `%40` decodes, to the `@` of a
/// revision target; strict BoardRef parsing rejects every other byte.
fn decode_render_target(raw: &str) -> Result<String, BoardError> {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if bytes.get(index..index + 3) != Some(b"%40") {
            return Err(invalid("malformed percent encoding in render target"));
        }
        decoded.push(b'@');
        index += 3;
    }
    String::from_utf8(decoded).map_err(|_| invalid("render target is not valid UTF-8"))
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChallengeRequest {
    api: u32,
    nonce: String,
}

/// The unauthenticated ownership proof: answer the client nonce with an HMAC
/// keyed by the token, so probes verify the listener without sending the token.
fn challenge_reply(
    request: &HttpRequest,
    guard: &WebGuard,
) -> Result<serde_json::Value, BoardError> {
    let challenge: ChallengeRequest = decode(request)?;
    ApiRequest {
        api: challenge.api,
    }
    .validate()?;
    let nonce = ChallengeNonce::from_hex(&challenge.nonce)
        .map_err(|error| invalid(error.to_string()))?;
    Ok(serde_json::json!({
        "api": BOARD_API,
        "proof": guard.challenge_proof(&nonce),
    }))
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
