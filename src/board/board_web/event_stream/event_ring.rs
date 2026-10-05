//! Bounded framed-event ring: one filler writes, every stream thread replays.
//! Frames are pre-built SSE bytes; plan relevance is annotated after push so
//! filtered subscribers replay from the ring without their own database reads.

mod ring_drain;
mod ring_plans;

pub(super) use ring_drain::Drain;
use super::REPLAY_LIMIT;
use crate::board::board_ids::{EventSeq, PlanId};
use ring_plans::PlanLease;
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
};

/// One pre-framed `event: board` payload plus the plans annotated as relevant.
pub(super) struct RingFrame {
    pub(super) seq: EventSeq,
    pub(super) plans: Vec<PlanId>,
    pub(super) bytes: Arc<[u8]>,
}

#[derive(Clone)]
pub(super) struct EventRing(Arc<RingSignal>);

struct RingSignal {
    state: Mutex<RingCore>,
    changed: Condvar,
}

#[derive(Default)]
struct RingCore {
    frames: VecDeque<RingFrame>,
    /// Freshest database watermark observed by the filler.
    latest: EventSeq,
    /// Events through here have been framed (or evicted); the fill cursor.
    filled: EventSeq,
    generation: u64,
    unavailable: bool,
    stopped: bool,
    plans: Vec<PlanTrack>,
}

struct PlanTrack {
    plan: PlanId,
    /// Frames in `(annotated, filled]` carry this plan's relevance already.
    annotated: EventSeq,
    subscribers: usize,
    failed: bool,
}

impl EventRing {
    pub(super) fn new() -> Self {
        Self(Arc::new(RingSignal {
            state: Mutex::new(RingCore::default()),
            changed: Condvar::new(),
        }))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RingCore> {
        self.0
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn bump(&self, state: &mut RingCore) {
        state.generation = state.generation.wrapping_add(1);
        self.0.changed.notify_all();
    }

    /// The observed watermark and the fill cursor for regression checks.
    pub(super) fn fill_snapshot(&self) -> (EventSeq, EventSeq) {
        let state = self.lock();
        (state.latest, state.filled)
    }

    pub(super) fn filled_seq(&self) -> EventSeq {
        self.lock().filled
    }

    /// Truncated fills leave the ring behind the observed watermark.
    pub(super) fn behind(&self) -> bool {
        let state = self.lock();
        state.latest > state.filled
    }

    pub(super) fn has_due_plans(&self) -> bool {
        let state = self.lock();
        state
            .plans
            .iter()
            .any(|track| track.subscribers > 0 && !track.failed && track.annotated < state.filled)
    }

    pub(super) fn due_plans(&self) -> Vec<(PlanId, EventSeq)> {
        let state = self.lock();
        state
            .plans
            .iter()
            .filter(|track| {
                track.subscribers > 0 && !track.failed && track.annotated < state.filled
            })
            .map(|track| (track.plan, track.annotated))
            .collect()
    }

    /// Appends a contiguous batch; `truncated` batches leave `filled` at the
    /// last pushed frame so the filler continues where the read stopped.
    pub(super) fn apply_events(
        &self,
        latest: EventSeq,
        frames: Vec<RingFrame>,
        truncated: bool,
    ) {
        let mut state = self.lock();
        let pushed = push_frames(&mut state, frames);
        state.latest = latest;
        state.filled = match (truncated, pushed) {
            (true, Some(seq)) => seq,
            (true, None) => state.filled,
            (false, _) => latest,
        };
        self.bump(&mut state);
    }

    /// Replaces the window outright (startup seed, restored database, or a
    /// burst beyond capacity); every tracked plan re-annotates the new window.
    pub(super) fn apply_reseed(
        &self,
        latest: EventSeq,
        frames: Vec<RingFrame>,
        truncated: bool,
    ) {
        let mut state = self.lock();
        state.frames.clear();
        let pushed = push_frames(&mut state, frames);
        state.latest = latest;
        state.filled = match (truncated, pushed) {
            (true, Some(seq)) => seq,
            (true, None) => state.filled,
            (false, _) => latest,
        };
        let covered = covered_of(&state);
        for track in &mut state.plans {
            track.annotated = track.annotated.min(covered);
        }
        self.bump(&mut state);
    }

    /// Marks frames relevant to `plan` and advances its annotation watermark.
    pub(super) fn apply_annotation(&self, plan: PlanId, seqs: Vec<EventSeq>) {
        let mut state = self.lock();
        let RingCore {
            frames,
            plans,
            filled,
            ..
        } = &mut *state;
        let Some(track) = plans
            .iter_mut()
            .find(|track| track.plan == plan && track.subscribers > 0 && !track.failed)
        else {
            return;
        };
        for seq in seqs {
            if let Ok(index) = frames.binary_search_by_key(&seq, |frame| frame.seq) {
                let frame = &mut frames[index];
                if !frame.plans.contains(&plan) {
                    frame.plans.push(plan);
                }
            }
        }
        track.annotated = *filled;
        self.bump(&mut state);
    }

    /// `require_plan` rejected the filter; its subscribers close like a failed
    /// first read did when streams queried the database themselves.
    pub(super) fn fail_plan(&self, plan: PlanId) {
        let mut state = self.lock();
        if let Some(track) = state
            .plans
            .iter_mut()
            .find(|track| track.plan == plan && !track.failed)
        {
            track.failed = true;
            self.bump(&mut state);
        }
    }

    /// Registers a filter; a previously failed plan retries (it may exist now).
    pub(super) fn subscribe_plan(&self, plan: PlanId) -> PlanLease {
        {
            let mut state = self.lock();
            let covered = covered_of(&state);
            let retrying;
            match state.plans.iter_mut().find(|track| track.plan == plan) {
                Some(track) => {
                    retrying = track.failed;
                    if retrying {
                        track.failed = false;
                        track.annotated = covered;
                    }
                    track.subscribers += 1;
                }
                None => {
                    retrying = false;
                    state.plans.push(PlanTrack {
                        plan,
                        annotated: covered,
                        subscribers: 1,
                        failed: false,
                    });
                }
            }
            if retrying {
                self.bump(&mut state);
            }
        }
        PlanLease {
            ring: self.clone(),
            plan,
        }
    }

    pub(super) fn mark_unavailable(&self) {
        let mut state = self.lock();
        if !state.unavailable {
            state.unavailable = true;
            self.bump(&mut state);
        }
    }

    pub(super) fn publish_available(&self) {
        let mut state = self.lock();
        if state.unavailable {
            state.unavailable = false;
            self.bump(&mut state);
        }
    }

    pub(super) fn stop(&self) {
        let mut state = self.lock();
        state.stopped = true;
        self.0.changed.notify_all();
    }

    /// The filler stopped (spawn failure, poisoned feed, or shutdown); new
    /// subscriptions must refuse instead of serving a dead ring.
    pub(super) fn stopped(&self) -> bool {
        self.lock().stopped
    }
}

/// The oldest cursor the ring can still serve fully.
fn covered_of(state: &RingCore) -> EventSeq {
    state
        .frames
        .front()
        .map_or(state.filled, |frame| EventSeq::new(frame.seq.get() - 1))
}

fn push_frames(state: &mut RingCore, frames: Vec<RingFrame>) -> Option<EventSeq> {
    let mut last = None;
    for frame in frames {
        last = Some(frame.seq);
        state.frames.push_back(frame);
    }
    while state.frames.len() > REPLAY_LIMIT {
        state.frames.pop_front();
    }
    last
}
