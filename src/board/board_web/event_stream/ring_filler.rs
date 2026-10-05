//! One feeder thread turns poller wakeups into shared-ring fills, so N streams
//! cost one event read per sequence change instead of one per stream per wake.

use super::{
    FEED_READ_LIMIT, FeedReader, REPLAY_LIMIT, ReplayBatch, event_frame,
    event_ring::{EventRing, RingFrame},
    sequence_poller::{POLL_INTERVAL, SequenceWake, WakeResult},
};
use crate::board::{
    board_ids::EventSeq,
    board_protocol::{BoardError, BoardErrorCode, EventRecord},
};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Instant,
};

enum FillProgress {
    CaughtUp,
    More,
}

/// Stops the filler when the last `EventStreams` clone (including per-stream
/// thread handles) drops; the ring is stopped first so serve threads exit.
pub(super) struct RingFiller {
    stop: Arc<AtomicBool>,
    wake: SequenceWake,
    thread: Option<JoinHandle<()>>,
}

impl RingFiller {
    pub(super) fn start(reader: FeedReader, wake: SequenceWake, ring: EventRing) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let spawned = {
            let stop = Arc::clone(&stop);
            let wake = wake.clone();
            let ring = ring.clone();
            thread::Builder::new()
                .name("board-ring-filler".into())
                .spawn(move || run_filler(reader, wake, ring, stop))
        };
        let thread = match spawned {
            Ok(thread) => Some(thread),
            // No filler means no fills; stop the ring so streams refuse cleanly.
            Err(_) => {
                ring.stop();
                None
            }
        };
        Self {
            stop,
            wake,
            thread,
        }
    }
}

impl Drop for RingFiller {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.wake.poke();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_filler(reader: FeedReader, wake: SequenceWake, ring: EventRing, stop: Arc<AtomicBool>) {
    let mut filled_at: Option<u64> = None;
    let mut retry_at: Option<Instant> = None;
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        if !wake.available() {
            ring.mark_unavailable();
            wake.wait_recovery(POLL_INTERVAL);
            continue;
        }
        let generation = wake.generation();
        // Idle watches never read: fills run on the first pass, on poller or
        // registration wakeups, while a burst is mid-fill, or to retry errors.
        let due = filled_at != Some(generation)
            || ring.has_due_plans()
            || retry_at.is_some_and(|at| Instant::now() >= at);
        if due {
            match catch_unwind(AssertUnwindSafe(|| fill_once(&reader, &ring))) {
                Ok(Ok(FillProgress::More)) => continue,
                Ok(Ok(FillProgress::CaughtUp)) => {
                    ring.publish_available();
                    filled_at = Some(generation);
                    retry_at = None;
                }
                Ok(Err(_)) => {
                    ring.mark_unavailable();
                    filled_at = Some(generation);
                    retry_at = Some(Instant::now() + POLL_INTERVAL);
                }
                // A panicking feed is poisoned; the filler gives up like a
                // panicked connection that must not be lent out again.
                Err(_) => {
                    ring.mark_unavailable();
                    break;
                }
            }
        }
        if stop.load(Ordering::Acquire) {
            break;
        }
        if wake.wait(generation, POLL_INTERVAL) == WakeResult::Stopped {
            break;
        }
    }
    ring.stop();
}

/// One pass: append or reseed the unfiltered window, then annotate relevance
/// for every due plan. No ring lock is held while the feed closure reads.
fn fill_once(reader: &FeedReader, ring: &EventRing) -> Result<FillProgress, BoardError> {
    let (observed, after) = ring.fill_snapshot();
    let batch = reader(after, None, FEED_READ_LIMIT)?;
    validate_batch(&batch, after)?;
    // Any watermark regression means a restored database; a burst beyond the
    // window reseeds as well. Both replace the ring with the newest page.
    if batch.latest < observed || batch.latest.get().saturating_sub(after.get()) > REPLAY_LIMIT as u64
    {
        // A restored database or a burst beyond the window reseeds the newest page.
        let from = EventSeq::new(batch.latest.get().saturating_sub(REPLAY_LIMIT as u64));
        let seed = reader(from, None, FEED_READ_LIMIT)?;
        validate_batch(&seed, from)?;
        let truncated = seed.events.len() >= FEED_READ_LIMIT;
        ring.apply_reseed(seed.latest, frames(seed.events)?, truncated);
    } else {
        let truncated = batch.events.len() >= FEED_READ_LIMIT;
        ring.apply_events(batch.latest, frames(batch.events)?, truncated);
    }
    let filled = ring.filled_seq();
    for (plan, annotated) in ring.due_plans() {
        match reader(annotated, Some(plan), FEED_READ_LIMIT) {
            Ok(batch) => ring.apply_annotation(plan, relevant_through(&batch, filled)),
            // One plan's relevance read failed (unknown plan or a read
            // error): fail only that plan's subscribers and keep filling.
            Err(_) => ring.fail_plan(plan),
        }
    }
    Ok(if ring.behind() {
        FillProgress::More
    } else {
        FillProgress::CaughtUp
    })
}

fn frames(events: Vec<EventRecord>) -> Result<Vec<RingFrame>, BoardError> {
    let mut frames = Vec::with_capacity(events.len());
    for event in events {
        let bytes = event_frame("board", Some(event.seq), &event)
            .map_err(|error| BoardError::new(BoardErrorCode::BoardUnavailable, error.to_string()))?;
        frames.push(RingFrame {
            seq: event.seq,
            plans: Vec::new(),
            bytes: bytes.into(),
        });
    }
    Ok(frames)
}

/// Relevance reads race new writes; only seqs already framed count this pass.
fn relevant_through(batch: &ReplayBatch, filled: EventSeq) -> Vec<EventSeq> {
    batch
        .events
        .iter()
        .map(|event| event.seq)
        .filter(|seq| *seq <= filled)
        .collect()
}

fn validate_batch(batch: &ReplayBatch, after: EventSeq) -> Result<(), BoardError> {
    batch
        .validate(after)
        .map_err(|error| BoardError::new(BoardErrorCode::BoardUnavailable, error.to_string()))
}
