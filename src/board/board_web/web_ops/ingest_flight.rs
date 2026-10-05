//! One ingest relay flight at a time; joiners share the leader's ticket.
/// One router ingest relay runs at a time; later posts join the running
/// scan's ticket and dirty the flight so the relay reruns after the scan.
pub(crate) struct IngestFlight {
    running: std::sync::atomic::AtomicBool,
    dirty: std::sync::atomic::AtomicBool,
    ticket: std::sync::Mutex<String>,
}

impl Default for IngestFlight {
    fn default() -> Self {
        Self {
            running: std::sync::atomic::AtomicBool::new(false),
            dirty: std::sync::atomic::AtomicBool::new(false),
            ticket: std::sync::Mutex::new(String::new()),
        }
    }
}

/// A POST's share of a flight: the leader runs the relay, joiners wait on
/// the running scan's ticket for the receipt the relay will publish.
pub(crate) struct IngestClaim {
    pub ticket: String,
    pub leads: bool,
}

impl IngestFlight {
    /// Claims the relay slot with a fresh ticket, or joins the running
    /// flight's ticket while dirtying it for a rerun. The mutex serializes
    /// the claim so a joiner always reads the current flight's ticket.
    pub(crate) fn begin(&self) -> IngestClaim {
        let mut ticket = self
            .ticket
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if !self.running.swap(true, std::sync::atomic::Ordering::AcqRel) {
            self.dirty.store(false, std::sync::atomic::Ordering::Release);
            *ticket = uuid::Uuid::new_v4().to_string();
            IngestClaim {
                ticket: ticket.clone(),
                leads: true,
            }
        } else {
            self.dirty.store(true, std::sync::atomic::Ordering::Release);
            IngestClaim {
                ticket: ticket.clone(),
                leads: false,
            }
        }
    }

    /// Takes a pending rerun request; concurrent joins coalesce into one.
    pub(crate) fn take_dirty(&self) -> bool {
        self.dirty.swap(false, std::sync::atomic::Ordering::AcqRel)
    }

    pub(crate) fn finish(&self) {
        self.running.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Runs the claimed flight's scans, rerunning while mid-scan POSTs dirtied
/// it; the guard holds the slot across reruns and releases it on return.
pub(crate) fn relay_flight(flight: &IngestFlight, mut scan: impl FnMut()) {
    let _guard = IngestFlightGuard::new(flight);
    loop {
        scan();
        if !flight.take_dirty() {
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
}

impl Drop for IngestFlightGuard<'_> {
    fn drop(&mut self) {
        self.0.finish();
    }
}
