//! Event route: stream subscriptions and cursor query parsing.
use super::invalid;
use super::route_replies::{send_board_error, send_error};
use super::super::{
    event_stream::{EventStreams, StreamPermit, StreamRequest},
    http_wire::HttpRequest,
};
use crate::board::board_protocol::BoardError;
use std::net::TcpStream;

/// A refused spawn (capacity reached, thread spawn failed) still answers the
/// subscriber: reserve failures map to 503 with Retry-After; a failed handoff
/// answers 503 on the returned socket instead of dropping it silently.
pub(super) fn stream_subscription(
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

pub(super) fn stream_request(request: &HttpRequest) -> Result<StreamRequest, BoardError> {
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
