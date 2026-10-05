//! One ingest relay flight at a time; mid-scan posts take the next ticket.
/// One router ingest relay runs at a time; a POST landing mid-scan takes
/// the next ticket and dirties the flight so the relay reruns under it.
pub(crate) struct IngestFlight {
    state: std::sync::Mutex<FlightState>,
}

struct FlightState {
    running: bool,
    dirty: bool,
    /// The running scan's ticket; only its receipt completes the leader.
    ticket: String,
    /// The rerun's ticket while `dirty`; mid-scan posts share it.
    next: String,
}

impl Default for IngestFlight {
    fn default() -> Self {
        Self {
            state: std::sync::Mutex::new(FlightState {
                running: false,
                dirty: false,
                ticket: String::new(),
                next: String::new(),
            }),
        }
    }
}

/// A POST's share of a flight: the leader runs the relay, joiners wait on
/// the next ticket for the rerun receipt the relay will publish.
pub(crate) struct IngestClaim {
    pub ticket: String,
    pub leads: bool,
}

impl IngestFlight {
    /// Claims the relay slot with a fresh ticket, or takes the next ticket
    /// while dirtying the flight for one rerun. The mutex serializes the
    /// claim so joiners during one scan share a single rerun ticket.
    pub(crate) fn begin(&self) -> IngestClaim {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if !state.running {
            state.running = true;
            state.dirty = false;
            state.ticket = uuid::Uuid::new_v4().to_string();
            state.next.clear();
            IngestClaim {
                ticket: state.ticket.clone(),
                leads: true,
            }
        } else {
            if !state.dirty {
                state.dirty = true;
                state.next = uuid::Uuid::new_v4().to_string();
            }
            IngestClaim {
                ticket: state.next.clone(),
                leads: false,
            }
        }
    }

    /// The running scan's ticket; the relay reads it fresh every scan so
    /// each rerun publishes under its own next ticket.
    pub(crate) fn current_ticket(&self) -> String {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .ticket
            .clone()
    }

    /// Settles a finished scan: a dirtied flight promotes the next ticket
    /// for exactly one rerun, a clean flight releases the slot. Clearing
    /// `running` and consuming `dirty` is one atomic step under the mutex,
    /// so a landing POST either queues its rerun or leads the next flight.
    fn settle(&self) -> Option<String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.dirty {
            state.dirty = false;
            state.ticket = std::mem::take(&mut state.next);
            Some(state.ticket.clone())
        } else {
            state.running = false;
            None
        }
    }

    /// Releases the slot after a spawn failure or panic; clean scans settle.
    pub(crate) fn finish(&self) {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .running = false;
    }
}

/// Runs the claimed flight's scans, rerunning while mid-scan POSTs dirtied
/// it; the guard holds the slot across reruns and releases it on return.
pub(crate) fn relay_flight(flight: &IngestFlight, mut scan: impl FnMut()) {
    let guard = IngestFlightGuard::new(flight);
    loop {
        scan();
        if flight.settle().is_none() {
            guard.disarm();
            return;
        }
    }
}

/// Returns the relay slot on drop, including panic paths.
pub(crate) struct IngestFlightGuard<'a>(&'a IngestFlight);

impl<'a> IngestFlightGuard<'a> {
    pub(crate) fn new(flight: &'a IngestFlight) -> Self {
        Self(flight)
    }

    /// Forgets a cleanly settled flight: the slot already released, and a
    /// new flight may have claimed it, so the drop must not touch it.
    fn disarm(self) {
        std::mem::forget(self);
    }
}

impl Drop for IngestFlightGuard<'_> {
    fn drop(&mut self) {
        self.0.finish();
    }
}
