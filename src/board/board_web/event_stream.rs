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
    collections::VecDeque,
    io,
    net::TcpStream,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub const STREAM_LIMIT: usize = 32;
pub const REPLAY_LIMIT: usize = 500;
pub const FEED_READ_LIMIT: usize = REPLAY_LIMIT + 1;
/// Ingest frames kept for lagging subscribers; older receipts are dropped.
const INGEST_LOG_LIMIT: usize = 8;
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
    ingest: Arc<Mutex<IngestLog>>,
}

/// Completed ingest relays, newest last; each subscriber drains by sequence.
#[derive(Default)]
struct IngestLog {
    next: u64,
    frames: VecDeque<(u64, Vec<u8>)>,
}

/// Dropping the permit releases capacity on return, disconnect, or unwinding.
#[derive(Debug)]
pub struct StreamPermit {
    active: Arc<AtomicUsize>,
}

impl Drop for StreamPermit {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A refused handoff: the socket returns to the caller for a 503 reply, and
/// the dropped permit has already released its slot.
#[derive(Debug)]
pub struct StreamRefusal {
    pub socket: TcpStream,
    pub error: io::Error,
}

impl EventStreams {
    pub fn new(reader: FeedReader, wake: SequenceWake) -> Self {
        Self {
            reader,
            wake,
            active: Arc::new(AtomicUsize::new(0)),
            ingest: Arc::new(Mutex::new(IngestLog::default())),
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

    /// Moves the connection out of its HTTP worker; a failed handoff returns
    /// the socket so the caller can answer 503, releasing the permit's slot.
    pub fn spawn(
        &self,
        socket: TcpStream,
        request: StreamRequest,
        permit: StreamPermit,
    ) -> Result<(), StreamRefusal> {
        if !Arc::ptr_eq(&self.active, &permit.active) {
            return Err(StreamRefusal {
                socket,
                error: io::Error::new(io::ErrorKind::InvalidInput, "foreign stream permit"),
            });
        }
        let cloned = match socket.try_clone() {
            Ok(cloned) => cloned,
            Err(error) => return Err(StreamRefusal { socket, error }),
        };
        let streams = self.clone();
        let spawned = thread::Builder::new()
            .name("board-event-stream".into())
            .spawn(move || {
                let _permit = permit;
                let _ = streams.serve(cloned, request);
            });
        match spawned {
            // The thread owns the clone; the original closes on drop here.
            Ok(_) => Ok(()),
            // A failed spawn drops the closure with its clone and permit.
            Err(error) => Err(StreamRefusal { socket, error }),
        }
    }

    pub fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }

    /// Records a completed ingest relay for broadcast; subscribers send the
    /// receipts sequenced after their subscription, newest frames kept.
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

    fn ingest_sequence(&self) -> u64 {
        self.ingest
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .next
    }

    /// Sends ingest receipts sequenced after `after_seq`; returns the new cursor.
    fn send_ingest_frames(&self, socket: &mut TcpStream, after_seq: u64) -> io::Result<u64> {
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

    fn serve(&self, mut socket: TcpStream, request: StreamRequest) -> io::Result<()> {
        if !self.wake.available() {
            return Ok(());
        }
        socket.set_nodelay(true)?;
        socket.set_read_timeout(Some(Duration::from_millis(1)))?;
        stream_socket::send(&mut socket, stream_socket::RESPONSE_HEADERS, SEND_DEADLINE)?;
        let mut ingest_after = self.ingest_sequence();
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
                ingest_after = self.send_ingest_frames(&mut socket, ingest_after)?;
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

/// Ingest receipts ride the stream as a side frame: no `id`, so the board
/// event cursor never advances on relay completions.
fn ingest_frame(value: &serde_json::Value) -> io::Result<Vec<u8>> {
    event_frame("ingest", None, value)
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
