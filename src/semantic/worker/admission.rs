use super::protocol::{MAX_INPUTS, MAX_RAW_INPUT_BYTES};
use anyhow::{Result, bail};
use std::sync::{Arc, Mutex};

pub const MAX_ADMITTED: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdmissionLimits {
    pub max_admitted: usize,
    pub max_raw_input_bytes: usize,
    pub max_inputs: usize,
}

impl Default for AdmissionLimits {
    fn default() -> Self {
        Self {
            max_admitted: MAX_ADMITTED,
            max_raw_input_bytes: MAX_RAW_INPUT_BYTES,
            max_inputs: MAX_INPUTS,
        }
    }
}

#[derive(Default)]
struct AdmissionState {
    queued: usize,
    inflight: usize,
    raw_input_bytes: usize,
}

#[derive(Clone)]
pub struct AdmissionController {
    limits: AdmissionLimits,
    state: Arc<Mutex<AdmissionState>>,
}

impl Default for AdmissionController {
    fn default() -> Self {
        Self::new(AdmissionLimits::default())
    }
}

impl AdmissionController {
    pub fn new(limits: AdmissionLimits) -> Self {
        Self {
            limits,
            state: Arc::new(Mutex::new(AdmissionState::default())),
        }
    }

    pub fn try_admit(&self, inputs: usize, raw_input_bytes: usize) -> Result<AdmissionTicket> {
        if inputs > self.limits.max_inputs {
            bail!(
                "semantic_admission: at most {} inputs per batch",
                self.limits.max_inputs
            );
        }
        if raw_input_bytes > self.limits.max_raw_input_bytes {
            bail!(
                "semantic_admission: raw inputs exceed {} bytes",
                self.limits.max_raw_input_bytes
            );
        }
        let mut state = self.state.lock().expect("admission state poisoned");
        if state.queued.saturating_add(state.inflight) >= self.limits.max_admitted {
            bail!("semantic_admission: 64 batches already queued or in flight");
        }
        let Some(total) = state.raw_input_bytes.checked_add(raw_input_bytes) else {
            bail!("semantic_admission: raw input byte counter overflow");
        };
        if total > self.limits.max_raw_input_bytes {
            bail!(
                "semantic_admission: raw inputs exceed {} bytes including in-flight batches",
                self.limits.max_raw_input_bytes
            );
        }
        state.queued += 1;
        state.raw_input_bytes = total;
        Ok(AdmissionTicket {
            state: Arc::clone(&self.state),
            raw_input_bytes,
            started: false,
        })
    }

    pub fn counts(&self) -> (usize, usize, usize) {
        let state = self.state.lock().expect("admission state poisoned");
        (state.queued, state.inflight, state.raw_input_bytes)
    }
}

pub struct AdmissionTicket {
    state: Arc<Mutex<AdmissionState>>,
    raw_input_bytes: usize,
    started: bool,
}

impl AdmissionTicket {
    pub fn begin(mut self) -> InflightTicket {
        let mut state = self.state.lock().expect("admission state poisoned");
        state.queued = state.queued.saturating_sub(1);
        state.inflight += 1;
        self.started = true;
        InflightTicket {
            state: Arc::clone(&self.state),
            raw_input_bytes: self.raw_input_bytes,
        }
    }
}

impl Drop for AdmissionTicket {
    fn drop(&mut self) {
        if self.started {
            return;
        }
        let mut state = self.state.lock().expect("admission state poisoned");
        state.queued = state.queued.saturating_sub(1);
        state.raw_input_bytes = state.raw_input_bytes.saturating_sub(self.raw_input_bytes);
    }
}

pub struct InflightTicket {
    state: Arc<Mutex<AdmissionState>>,
    raw_input_bytes: usize,
}

impl Drop for InflightTicket {
    fn drop(&mut self) {
        let mut state = self.state.lock().expect("admission state poisoned");
        state.inflight = state.inflight.saturating_sub(1);
        state.raw_input_bytes = state.raw_input_bytes.saturating_sub(self.raw_input_bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_counts_queued_and_inflight_and_releases_on_drop() -> Result<()> {
        let admission = AdmissionController::default();
        let ticket = admission.try_admit(1, 4)?;
        assert_eq!(admission.counts(), (1, 0, 4));
        let inflight = ticket.begin();
        assert_eq!(admission.counts(), (0, 1, 4));
        drop(inflight);
        assert_eq!(admission.counts(), (0, 0, 0));
        Ok(())
    }

    #[test]
    fn admission_rejects_more_than_sixty_four_batches() -> Result<()> {
        let admission = AdmissionController::default();
        let mut tickets = Vec::with_capacity(MAX_ADMITTED);
        for _ in 0..MAX_ADMITTED {
            tickets.push(admission.try_admit(1, 1)?);
        }
        assert!(admission.try_admit(1, 1).is_err());
        drop(tickets);
        assert_eq!(admission.counts(), (0, 0, 0));
        Ok(())
    }
}
