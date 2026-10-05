//! One stream thread per subscriber: replay, ingest frames, and keepalive.
use super::{
    AHEAD_CURSOR_GRACE, EventStreams, KEEPALIVE_INTERVAL, Resync, SEND_DEADLINE, StreamRequest,
    event_frame,
    event_ring::Drain,
    sequence_poller::{POLL_INTERVAL, WakeResult},
    stream_socket,
};
use crate::board::{board_ids::EventSeq, board_protocol::BoardErrorCode};
use std::{
    io,
    net::TcpStream,
    time::{Duration, Instant},
};

impl EventStreams {
    pub(super) fn serve(&self, mut socket: TcpStream, request: StreamRequest) -> io::Result<()> {
        // A poller lost between reserve and serve still answers: an empty
        // reply would read as a dropped connection, not a retryable outage.
        if !self.wake.available() {
            return unavailable_reply(&mut socket);
        }
        if let Some(plan) = request.plan {
            // Unknown plans are rejected before any 200: a 200-then-close
            // would reconnect forever. Other read errors fall through to
            // the filler's availability handling below.
            match (self.reader)(EventSeq::new(0), Some(plan), 1) {
                Err(error) if error.code == BoardErrorCode::InvalidReference => {
                    let body = serde_json::to_vec(&serde_json::json!({
                        "error": { "code": error.code.as_str(), "message": error.message }
                    }))
                    .map_err(io::Error::other)?;
                    super::super::http_wire::send_response(
                        &mut socket,
                        404,
                        "application/json",
                        &body,
                    )?;
                    return Ok(());
                }
                _ => {}
            }
        }
        socket.set_nodelay(true)?;
        socket.set_read_timeout(Some(Duration::from_millis(1)))?;
        stream_socket::send(&mut socket, stream_socket::RESPONSE_HEADERS, SEND_DEADLINE)?;
        // The lease keeps relevance reads alive; the poke wakes the filler to
        // annotate the window before this subscriber's first drain needs it.
        let _plan_lease = request.plan.map(|plan| {
            let lease = self.ring.subscribe_plan(plan);
            self.wake.poke();
            lease
        });
        let mut ingest_after = self.ingest_replay_start();
        let mut cursor = request.after;
        let mut last_send = Instant::now();
        let mut ahead_since: Option<Instant> = None;
        let mut recovering_since: Option<Instant> = None;
        loop {
            if !self.wake.available() {
                return Ok(());
            }
            match self.ring.drain(cursor, request.plan) {
                Drain::Stopped | Drain::PlanFailed => return Ok(()),
                Drain::Unavailable => {
                    // A wake outage closes promptly; a recovered poller
                    // republishes the ring within moments, so bridge that window.
                    if !self.wake.available() {
                        return Ok(());
                    }
                    let waited = *recovering_since.get_or_insert_with(Instant::now);
                    let remaining = AHEAD_CURSOR_GRACE.saturating_sub(waited.elapsed());
                    if remaining.is_zero() {
                        return Ok(());
                    }
                    self.ring.wait_recovery(remaining);
                }
                Drain::Gap { latest } => {
                    let frame = event_frame(
                        "resync",
                        None,
                        &Resync {
                            reason: "replay_gap",
                            latest,
                        },
                    )?;
                    return stream_socket::send(&mut socket, &frame, SEND_DEADLINE);
                }
                Drain::Ahead { latest, generation } => {
                    let waited = *ahead_since.get_or_insert_with(Instant::now);
                    let remaining = AHEAD_CURSOR_GRACE.saturating_sub(waited.elapsed());
                    if remaining.is_zero() {
                        // Contention can starve the filler past the grace
                        // while fresh writes wait; poke it awake, then
                        // re-read the watermark directly from the feed before
                        // declaring a genuinely ahead cursor.
                        self.wake.poke();
                        let fresh = (self.reader)(cursor, None, 1)
                            .map(|batch| batch.latest)
                            .unwrap_or(latest);
                        if fresh >= cursor {
                            ahead_since = None;
                            continue;
                        }
                        let frame = event_frame(
                            "resync",
                            None,
                            &Resync {
                                reason: "cursor_ahead",
                                latest,
                            },
                        )?;
                        return stream_socket::send(&mut socket, &frame, SEND_DEADLINE);
                    }
                    match self.ring.wait(generation, remaining.min(POLL_INTERVAL)) {
                        WakeResult::Unavailable | WakeResult::Stopped => return Ok(()),
                        WakeResult::Changed | WakeResult::Timeout => {}
                    }
                }
                Drain::PendingPlan { generation } => {
                    match self.ring.wait(generation, POLL_INTERVAL) {
                        WakeResult::Unavailable | WakeResult::Stopped => return Ok(()),
                        WakeResult::Changed | WakeResult::Timeout => {}
                    }
                }
                Drain::Frames {
                    frames,
                    cursor: target,
                    generation,
                } => {
                    ahead_since = None;
                    recovering_since = None;
                    for frame in &frames {
                        if !self.wake.available() {
                            return Ok(());
                        }
                        stream_socket::send(&mut socket, frame, SEND_DEADLINE)?;
                        last_send = Instant::now();
                    }
                    cursor = target;
                    loop {
                        if !stream_socket::peer_connected(&socket)? {
                            return Ok(());
                        }
                        ingest_after = self.send_ingest_frames(&mut socket, ingest_after)?;
                        if last_send.elapsed() >= KEEPALIVE_INTERVAL {
                            stream_socket::send(&mut socket, b": keepalive\n\n", SEND_DEADLINE)?;
                            last_send = Instant::now();
                        }
                        let timeout = KEEPALIVE_INTERVAL
                            .saturating_sub(last_send.elapsed())
                            .min(POLL_INTERVAL);
                        match self.ring.wait(generation, timeout) {
                            WakeResult::Changed => break,
                            WakeResult::Timeout => {}
                            WakeResult::Unavailable | WakeResult::Stopped => return Ok(()),
                        }
                    }
                }
            }
        }
    }
}

/// Pre-200 refusal for a poller lost after admission; matches the route 503
/// envelope so reconnecting subscribers back off instead of spinning.
fn unavailable_reply(socket: &mut TcpStream) -> io::Result<()> {
    let body = serde_json::to_vec(&serde_json::json!({
        "error": {
            "code": "board_unavailable",
            "message": "event sequence poller unavailable",
        }
    }))
    .map_err(io::Error::other)?;
    super::super::http_wire::send_unavailable(socket, 1, &body)
}
