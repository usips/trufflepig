//! Subscriber reads: one consistent drain decision per cursor.
use super::{EventRing, covered_of};
use super::super::sequence_poller::WakeResult;
use crate::board::board_ids::{EventSeq, PlanId};
use std::{sync::Arc, time::Duration};

/// One drain decision for a subscriber cursor.
pub(crate) enum Drain {
    Frames {
        frames: Vec<Arc<[u8]>>,
        cursor: EventSeq,
        generation: u64,
    },
    /// The cursor is above the observed watermark; the poller may still lag.
    Ahead { latest: EventSeq, generation: u64 },
    /// The cursor is older than the ring's oldest frame.
    Gap { latest: EventSeq },
    /// Plan relevance has not caught up with the ring yet.
    PendingPlan { generation: u64 },
    PlanFailed,
    Unavailable,
    Stopped,
}

impl EventRing {
    /// Reads one consistent drain decision; frame bytes are cloned under the
    /// lock so no socket write happens inside it.
    pub(crate) fn drain(&self, cursor: EventSeq, plan: Option<PlanId>) -> Drain {
        let state = self.lock();
        if state.stopped {
            return Drain::Stopped;
        }
        if state.unavailable {
            return Drain::Unavailable;
        }
        if cursor > state.latest {
            return Drain::Ahead {
                latest: state.latest,
                generation: state.generation,
            };
        }
        let covered = covered_of(&state);
        if cursor < covered {
            return Drain::Gap {
                latest: state.latest,
            };
        }
        if let Some(plan) = plan {
            match state
                .plans
                .iter()
                .find(|track| track.plan == plan && track.subscribers > 0)
            {
                Some(track) if track.failed => return Drain::PlanFailed,
                Some(track) if track.annotated < state.filled => {
                    return Drain::PendingPlan {
                        generation: state.generation,
                    };
                }
                Some(_) => {}
                None => return Drain::PlanFailed,
            }
        }
        let frames = state
            .frames
            .iter()
            .filter(|frame| frame.seq > cursor)
            .filter(|frame| plan.map_or(true, |plan| frame.plans.contains(&plan)))
            .map(|frame| Arc::clone(&frame.bytes))
            .collect();
        Drain::Frames {
            frames,
            // The cursor never moves backwards while a truncated fill catches up.
            cursor: state.filled.max(cursor),
            generation: state.generation,
        }
    }

    pub(crate) fn wait(&self, observed: u64, timeout: Duration) -> WakeResult {
        let state = self.lock();
        let (state, _) = self
            .0
            .changed
            .wait_timeout_while(state, timeout, |state| {
                !state.stopped && !state.unavailable && state.generation == observed
            })
            .unwrap_or_else(|poison| poison.into_inner());
        if state.stopped {
            WakeResult::Stopped
        } else if state.unavailable {
            WakeResult::Unavailable
        } else if state.generation != observed {
            WakeResult::Changed
        } else {
            WakeResult::Timeout
        }
    }

    /// Waits out a feed outage: returns when the ring recovers or stops.
    pub(crate) fn wait_recovery(&self, timeout: Duration) {
        let state = self.lock();
        let _ = self
            .0
            .changed
            .wait_timeout_while(state, timeout, |state| state.unavailable && !state.stopped)
            .unwrap_or_else(|poison| poison.into_inner());
    }
}
