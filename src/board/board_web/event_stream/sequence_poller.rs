//! A bounded cadence observes committed sequence changes without retaining readers.

use crate::board::{board_ids::EventSeq, board_protocol::BoardError};
use std::{
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::Duration,
};

pub(super) const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Performs only query-only max(seq), releasing its reader lease before return.
pub type SequenceReader = Arc<dyn Fn() -> Result<EventSeq, BoardError> + Send + Sync>;

#[derive(Clone)]
pub struct SequenceWake(Arc<SequenceSignal>);

struct SequenceSignal {
    state: Mutex<SequenceGeneration>,
    changed: Condvar,
}

#[derive(Default)]
struct SequenceGeneration {
    generation: u64,
    latest: EventSeq,
    unavailable: bool,
    stopped: bool,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum WakeResult {
    Changed,
    Timeout,
    Unavailable,
    Stopped,
}

pub struct SequencePoller {
    wake: SequenceWake,
    thread: Option<JoinHandle<()>>,
}

impl SequencePoller {
    pub fn start(reader: SequenceReader) -> io::Result<Self> {
        let wake = SequenceWake::new();
        let poll_wake = wake.clone();
        let thread = thread::Builder::new()
            .name("board-sequence-poller".into())
            .spawn(move || {
                loop {
                    if poll_wake.stopped() {
                        break;
                    }
                    match catch_unwind(AssertUnwindSafe(|| reader())) {
                        Ok(Ok(latest)) => poll_wake.publish(latest),
                        Ok(Err(_)) | Err(_) => poll_wake.mark_unavailable(),
                    }
                    // Outage notifications must not shorten the retry cadence.
                    if poll_wake.wait_for_poll() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            wake,
            thread: Some(thread),
        })
    }

    pub fn handle(&self) -> SequenceWake {
        self.wake.clone()
    }
}

impl Drop for SequencePoller {
    fn drop(&mut self) {
        self.wake.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl SequenceWake {
    pub(super) fn new() -> Self {
        Self(Arc::new(SequenceSignal {
            state: Mutex::new(SequenceGeneration::default()),
            changed: Condvar::new(),
        }))
    }

    pub(super) fn generation(&self) -> u64 {
        self.0
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .generation
    }

    fn stopped(&self) -> bool {
        self.0
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .stopped
    }

    pub(super) fn available(&self) -> bool {
        let state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        !state.stopped && !state.unavailable
    }

    pub(super) fn publish(&self, latest: EventSeq) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if latest != state.latest || state.unavailable {
            state.latest = latest;
            state.unavailable = false;
            state.generation = state.generation.wrapping_add(1);
            self.0.changed.notify_all();
        }
    }

    /// Wakes waiters without new data so the ring filler rechecks plan
    /// registrations; generation inequality is the only waiter signal.
    pub(super) fn poke(&self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.generation = state.generation.wrapping_add(1);
        self.0.changed.notify_all();
    }

    /// Waits out an outage: returns when polling recovers, stops, or times out.
    pub(super) fn wait_recovery(&self, timeout: Duration) {
        let state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let _ = self
            .0
            .changed
            .wait_timeout_while(state, timeout, |state| state.unavailable && !state.stopped)
            .unwrap_or_else(|poison| poison.into_inner());
    }

    pub(super) fn mark_unavailable(&self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if !state.unavailable {
            state.unavailable = true;
            state.generation = state.generation.wrapping_add(1);
            self.0.changed.notify_all();
        }
    }

    fn wait_for_poll(&self) -> bool {
        let state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let (state, _) = self
            .0
            .changed
            .wait_timeout_while(state, POLL_INTERVAL, |state| !state.stopped)
            .unwrap_or_else(|poison| poison.into_inner());
        state.stopped
    }

    pub(super) fn stop(&self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.stopped = true;
        self.0.changed.notify_all();
    }

    pub(super) fn wait(&self, observed: u64, timeout: Duration) -> WakeResult {
        let state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
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
}
