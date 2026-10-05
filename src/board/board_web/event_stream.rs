//! Ordered board-event replay from a shared ring fed by one dedicated reader.
//! Feed reads return owned snapshot records before any socket writes or waits.

mod event_ring;
mod ring_filler;
mod sequence_poller;
mod stream_socket;
#[cfg(test)]
mod tests;

pub use sequence_poller::{SequencePoller, SequenceWake};

use crate::board::{
    board_ids::{EventSeq, PlanId},
    board_protocol::{BoardError, BoardErrorCode, EventRecord},
};
use event_ring::{Drain, EventRing};
use ring_filler::RingFiller;
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
    fn ingest_replay_start(&self) -> u64 {
        let log = self
            .ingest
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        log.next.saturating_sub(log.frames.len() as u64)
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
                    super::http_wire::send_response(
                        &mut socket,
                        404,
                        "application/json",
                        &body,
                        true,
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
                Drain::Ahead {
                    latest,
                    generation,
                } => {
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
                    match self
                        .ring
                        .wait(generation, remaining.min(sequence_poller::POLL_INTERVAL))
                    {
                        WakeResult::Unavailable | WakeResult::Stopped => return Ok(()),
                        WakeResult::Changed | WakeResult::Timeout => {}
                    }
                }
                Drain::PendingPlan { generation } => {
                    match self.ring.wait(generation, sequence_poller::POLL_INTERVAL) {
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
                            .min(sequence_poller::POLL_INTERVAL);
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
