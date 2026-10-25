//! Ingest receipt log: completed relays ride streams as side frames.
use super::{EventStreams, INGEST_LOG_LIMIT, SEND_DEADLINE, event_frame, stream_socket};
use std::{collections::VecDeque, io, net::TcpStream};

/// Completed ingest relays, newest last; each subscriber drains by sequence.
#[derive(Default)]
pub(super) struct IngestLog {
    next: u64,
    frames: VecDeque<(u64, Vec<u8>)>,
}

impl EventStreams {
    /// Records a completed ingest relay for broadcast; subscribers drain
    /// every kept receipt and filter by flight ticket, newest frames kept.
    pub fn publish_ingest(&self, result: &serde_json::Value) {
        let Ok(frame) = ingest_frame(result) else {
            return;
        };
        let mut log = self
            .ingest
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        log.next += 1;
        let next = log.next;
        log.frames.push_back((next, frame));
        while log.frames.len() > INGEST_LOG_LIMIT {
            log.frames.pop_front();
        }
    }

    /// A (re)subscribing stream drains every kept receipt, including ones
    /// published while it was away; tab-side ticket filtering drops the
    /// flights the tab never joined.
    pub(super) fn ingest_replay_start(&self) -> u64 {
        let log = self
            .ingest
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        log.next.saturating_sub(log.frames.len() as u64)
    }

    /// Sends ingest receipts sequenced after `after_seq`; returns the new cursor.
    pub(super) fn send_ingest_frames(
        &self,
        socket: &mut TcpStream,
        after_seq: u64,
    ) -> io::Result<u64> {
        let frames: Vec<(u64, Vec<u8>)> = {
            let log = self
                .ingest
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            log.frames
                .iter()
                .filter(|(seq, _)| *seq > after_seq)
                .map(|(seq, frame)| (*seq, frame.clone()))
                .collect()
        };
        let mut cursor = after_seq;
        for (seq, frame) in frames {
            stream_socket::send(socket, &frame, SEND_DEADLINE)?;
            cursor = seq;
        }
        Ok(cursor)
    }
}

/// Ingest receipts ride the stream as a side frame: no `id`, so the board
/// event cursor never advances on relay completions.
fn ingest_frame(value: &serde_json::Value) -> io::Result<Vec<u8>> {
    event_frame("ingest", None, value)
}
