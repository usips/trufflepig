//! Public assets are empty of board data; every board read and write is authenticated.
mod route_events;
mod route_ingest;
mod route_render;
mod route_replies;
#[cfg(test)]
mod tests;
use super::{
    PUBLIC_DOM, PUBLIC_ENTRIES, PUBLIC_INGEST, PUBLIC_LRU, PUBLIC_MAIN, PUBLIC_PAGES, PUBLIC_PLAN_PAGE,
    PUBLIC_PROPOSAL_PAGE, PUBLIC_READER,
    PUBLIC_SEEN, PUBLIC_SHELL, PUBLIC_STREAM, PUBLIC_STREAM_ELECTION, PUBLIC_STREAM_PARSE,
    PUBLIC_STYLE, PUBLIC_TOKEN, PUBLIC_TRIAGE,
    PUBLIC_VIEWS, BoardWebState,
    http_wire::{self, HttpError, HttpMethod, HttpRequest},
    web_guard::{ChallengeNonce, RouteAccess, WebGuard},
    web_ops::{self, WebRequest},
};
use crate::board::board_protocol::{BOARD_API, BoardError, BoardErrorCode};
use route_events::{stream_request, stream_subscription};
use route_ingest::{ApiRequest, ingest_accepted};
use route_render::{decode_render_target, render_diff, render_plan, render_proposal};
use route_replies::{send_board_error, send_error, send_http_error, send_json_result};
use serde::Deserialize;
use std::{net::TcpStream, sync::Arc, time::Instant};

pub(super) fn handle(mut stream: TcpStream, accepted_at: Instant, state: &Arc<BoardWebState>) {
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
            );
        }
        (HttpMethod::Get, path) if public_asset(path).is_some() => {
            let (content_type, body) = public_asset(path).unwrap();
            let _ = http_wire::send_response(&mut stream, 200, content_type, body.as_bytes());
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

fn public_asset(path: &str) -> Option<(&str, &str)> {
    let script = match path {
        "/board_web_main.js" => PUBLIC_MAIN,
        "/board_dom.js" => PUBLIC_DOM,
        "/board_views.js" => PUBLIC_VIEWS,
        "/board_pages.js" => PUBLIC_PAGES,
        "/plan_page.js" => PUBLIC_PLAN_PAGE,
        "/proposal_page.js" => PUBLIC_PROPOSAL_PAGE,
        "/board_stream.js" => PUBLIC_STREAM,
        "/stream_election.js" => PUBLIC_STREAM_ELECTION,
        "/stream_parse.js" => PUBLIC_STREAM_PARSE,
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

fn invalid(message: impl Into<String>) -> BoardError {
    BoardError::new(BoardErrorCode::InvalidOptions, message)
}
