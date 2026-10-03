//! Ordered, bounded board event replay followed by coalesced sequence wakeups.
//! Feed readers return owned snapshot records before any socket writes or waits.

mod sequence_poller;
mod stream_socket;
#[cfg(test)]
mod tests;

pub use sequence_poller::{SequencePoller, SequenceReader, SequenceWake};

use crate::board::{
    board_ids::{EventSeq, PlanId},
    board_protocol::{BoardError, BoardErrorCode, EventRecord},
};
use sequence_poller::WakeResult;
use serde::Serialize;
use std::{
    io,
    net::TcpStream,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub const STREAM_LIMIT: usize = 32;
pub const REPLAY_LIMIT: usize = 500;
pub const FEED_READ_LIMIT: usize = REPLAY_LIMIT + 1;
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const SEND_DEADLINE: Duration = Duration::from_secs(5);

/// Both fields come from one read snapshot; events are ordered by increasing seq.
/// The reader proves plan relevance, including planless aggregate commit events.
#[derive(Clone, Debug)]
pub struct ReplayBatch {
    pub latest: EventSeq,
    pub events: Vec<EventRecord>,
}

/// The closure applies the plan filter, materializes the feed, and releases its lease.
pub type FeedReader =
    Arc<dyn Fn(EventSeq, Option<PlanId>, usize) -> Result<ReplayBatch, BoardError> + Send + Sync>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamRequest {
    pub after: EventSeq,
    pub plan: Option<PlanId>,
}

impl StreamRequest {
    /// A supplied Last-Event-ID wins, including when its value is malformed.
    pub fn parse(
        last_event_id: Option<&str>,
        after: Option<&str>,
        plan: Option<&str>,
    ) -> Result<Self, BoardError> {
        let after = last_event_id.or(after).unwrap_or("0");
        Ok(Self {
            after: EventSeq::parse(after).map_err(|_| {
                BoardError::new(BoardErrorCode::InvalidOptions, "invalid event cursor")
            })?,
            plan: plan.map(PlanId::parse).transpose().map_err(|_| {
                BoardError::new(BoardErrorCode::InvalidOptions, "invalid event plan filter")
            })?,
        })
    }
}

#[derive(Clone)]
pub struct EventStreams {
    reader: FeedReader,
    wake: SequenceWake,
    active: Arc<AtomicUsize>,
}

/// Dropping the permit releases capacity on return, disconnect, or unwinding.
pub struct StreamPermit {
    active: Arc<AtomicUsize>,
}

impl Drop for StreamPermit {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

impl EventStreams {
    pub fn new(reader: FeedReader, wake: SequenceWake) -> Self {
        Self {
            reader,
            wake,
            active: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Reserve only after request authentication and cursor parsing succeed.
    pub fn reserve(&self) -> Result<StreamPermit, BoardError> {
        if !self.wake.available() {
            return Err(BoardError::new(
                BoardErrorCode::BoardUnavailable,
                "event sequence poller unavailable",
            ));
        }
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < STREAM_LIMIT).then_some(active + 1)
            })
            .map_err(|_| {
                BoardError::new(BoardErrorCode::DaemonBusy, "event stream capacity reached")
            })?;
        Ok(StreamPermit {
            active: Arc::clone(&self.active),
        })
    }

    /// Moves the connection out of its HTTP worker; failed spawn drops the permit.
    pub fn spawn(
        &self,
        socket: TcpStream,
        request: StreamRequest,
        permit: StreamPermit,
    ) -> io::Result<()> {
        if !Arc::ptr_eq(&self.active, &permit.active) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "foreign stream permit",
            ));
        }
        let streams = self.clone();
        thread::Builder::new()
            .name("board-event-stream".into())
            .spawn(move || {
                let _permit = permit;
                let _ = streams.serve(socket, request);
            })
            .map(|_| ())
    }

    pub fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }

    fn serve(&self, mut socket: TcpStream, request: StreamRequest) -> io::Result<()> {
        if !self.wake.available() {
            return Ok(());
        }
        socket.set_nodelay(true)?;
        socket.set_read_timeout(Some(Duration::from_millis(1)))?;
        stream_socket::send(&mut socket, stream_socket::RESPONSE_HEADERS, SEND_DEADLINE)?;
        let mut after = request.after;
        let mut last_send = Instant::now();
        loop {
            if !self.wake.available() {
                return Ok(());
            }
            // Capturing before the read prevents missing a concurrent poller wakeup.
            let generation = self.wake.generation();
            let batch =
                (self.reader)(after, request.plan, FEED_READ_LIMIT).map_err(io::Error::other)?;
            if !self.wake.available() {
                return Ok(());
            }
            if let Some(reason) = batch.resync_reason(after) {
                let frame = event_frame(
                    "resync",
                    None,
                    &Resync {
                        reason,
                        latest: batch.latest,
                    },
                )?;
                return stream_socket::send(&mut socket, &frame, SEND_DEADLINE);
            }
            batch.validate(after)?;
            for event in &batch.events {
                if !self.wake.available() {
                    return Ok(());
                }
                let frame = event_frame("board", Some(event.seq), event)?;
                stream_socket::send(&mut socket, &frame, SEND_DEADLINE)?;
                last_send = Instant::now();
            }
            // The entire filtered snapshot is consumed only after successful sends.
            after = batch.latest;
            loop {
                if !stream_socket::peer_connected(&socket)? {
                    return Ok(());
                }
                if last_send.elapsed() >= KEEPALIVE_INTERVAL {
                    stream_socket::send(&mut socket, b": keepalive\n\n", SEND_DEADLINE)?;
                    last_send = Instant::now();
                }
                let timeout = KEEPALIVE_INTERVAL
                    .saturating_sub(last_send.elapsed())
                    .min(sequence_poller::POLL_INTERVAL);
                match self.wake.wait(generation, timeout) {
                    WakeResult::Changed => break,
                    WakeResult::Timeout => {}
                    WakeResult::Unavailable | WakeResult::Stopped => return Ok(()),
                }
            }
        }
    }
}

impl ReplayBatch {
    fn resync_reason(&self, after: EventSeq) -> Option<&'static str> {
        if after > self.latest {
            Some("cursor_ahead")
        } else if self.events.len() > REPLAY_LIMIT {
            Some("replay_gap")
        } else {
            None
        }
    }

    fn validate(&self, after: EventSeq) -> io::Result<()> {
        let mut previous = after;
        for event in &self.events {
            if event.seq <= previous || event.seq > self.latest {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid event snapshot",
                ));
            }
            previous = event.seq;
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct Resync {
    reason: &'static str,
    latest: EventSeq,
}

fn event_frame(event: &str, id: Option<EventSeq>, value: &impl Serialize) -> io::Result<Vec<u8>> {
    let json = serde_json::to_vec(value).map_err(io::Error::other)?;
    let mut frame = Vec::with_capacity(json.len() + event.len() + 64);
    frame.extend_from_slice(b"event: ");
    frame.extend_from_slice(event.as_bytes());
    frame.push(b'\n');
    if let Some(id) = id {
        frame.extend_from_slice(format!("id: {id}\n").as_bytes());
    }
    frame.extend_from_slice(b"data: ");
    frame.extend_from_slice(&json);
    frame.extend_from_slice(b"\n\n");
    Ok(frame)
}
