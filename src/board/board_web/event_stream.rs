//! Ordered board-event replay from a shared ring fed by one dedicated reader.
//! Feed reads return owned snapshot records before any socket writes or waits.

mod event_ring;
mod ring_filler;
mod sequence_poller;
mod stream_ingest;
mod stream_serve;
mod stream_socket;
#[cfg(test)]
mod tests;

pub use sequence_poller::{SequencePoller, SequenceWake};

use crate::board::{
    board_ids::{EventSeq, PlanId},
    board_protocol::{BoardError, BoardErrorCode, EventRecord},
};
use event_ring::EventRing;
use ring_filler::RingFiller;
use serde::Serialize;
use std::{
    io,
    net::TcpStream,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};
use stream_ingest::IngestLog;

pub const STREAM_LIMIT: usize = 32;
pub const REPLAY_LIMIT: usize = 500;
pub const FEED_READ_LIMIT: usize = REPLAY_LIMIT + 1;
/// Ingest frames kept for lagging subscribers; older receipts are dropped.
const INGEST_LOG_LIMIT: usize = 8;
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const SEND_DEADLINE: Duration = Duration::from_secs(5);
/// A cursor above the ring's watermark gets two poller ticks to arrive (fresh
/// writes lag the poller); a still-ahead cursor means a restored database.
const AHEAD_CURSOR_GRACE: Duration = Duration::from_millis(600);

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
    wake: SequenceWake,
    ring: EventRing,
    /// Pre-200 plan checks and direct watermark re-reads bypass the ring.
    reader: FeedReader,
    /// Keeps the filler alive until the last stream clone (and serve thread) drops.
    _filler: Arc<RingFiller>,
    active: Arc<AtomicUsize>,
    ingest: Arc<Mutex<IngestLog>>,
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
        let ring = EventRing::new();
        let filler = RingFiller::start(reader.clone(), wake.clone(), ring.clone());
        Self {
            wake,
            ring,
            reader,
            _filler: Arc::new(filler),
            active: Arc::new(AtomicUsize::new(0)),
            ingest: Arc::new(Mutex::new(IngestLog::default())),
        }
    }

    /// True once the filler has stopped; the ring never recovers
    /// in-process, so the serve loop exits for a systemd restart.
    pub fn ring_stopped(&self) -> bool {
        self.ring.stopped()
    }

    /// Reserve only after request authentication and cursor parsing succeed.
    pub fn reserve(&self) -> Result<StreamPermit, BoardError> {
        if self.ring.stopped() {
            return Err(BoardError::new(
                BoardErrorCode::BoardUnavailable,
                "event ring stopped",
            ));
        }
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

    #[cfg(test)]
    pub fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }
}

impl ReplayBatch {
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
