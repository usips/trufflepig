use std::time::{Duration, Instant};

const INITIAL_COOLDOWN: Duration = Duration::from_millis(500);
const MAX_COOLDOWN: Duration = Duration::from_secs(30);

/// Schedules lazy model-load retries after a failed initialization.
#[derive(Debug)]
pub(super) struct LoadRetry {
    retry_at: Option<Instant>,
    cooldown: Duration,
}

impl Default for LoadRetry {
    fn default() -> Self {
        Self {
            retry_at: None,
            cooldown: INITIAL_COOLDOWN,
        }
    }
}

impl LoadRetry {
    pub(super) fn is_ready(&self, now: Instant) -> bool {
        self.retry_at.is_none_or(|retry_at| now >= retry_at)
    }

    pub(super) fn failed(&mut self, now: Instant) {
        self.retry_at = Some(now + self.cooldown);
        self.cooldown = self.cooldown.saturating_mul(2).min(MAX_COOLDOWN);
    }

    pub(super) fn succeeded(&mut self) {
        self.retry_at = None;
        self.cooldown = INITIAL_COOLDOWN;
    }

    #[cfg(test)]
    pub(super) fn retry_at(&self) -> Option<Instant> {
        self.retry_at
    }

    #[cfg(test)]
    pub(super) fn cooldown(&self) -> Duration {
        self.cooldown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_load_waits_then_allows_one_retry_and_success_resets() {
        let started = Instant::now();
        let mut retry = LoadRetry::default();

        assert!(retry.is_ready(started));
        retry.failed(started);
        let retry_at = retry.retry_at().unwrap();
        assert!(!retry.is_ready(retry_at - Duration::from_nanos(1)));
        assert!(retry.is_ready(retry_at));

        retry.failed(retry_at);
        assert!(!retry.is_ready(retry_at + INITIAL_COOLDOWN));
        assert_eq!(retry.cooldown(), INITIAL_COOLDOWN * 4);

        retry.succeeded();
        assert!(retry.is_ready(started));
        assert_eq!(retry.cooldown(), INITIAL_COOLDOWN);
    }

    #[test]
    fn cooldown_is_bounded_after_repeated_no_gpu_failures() {
        let mut retry = LoadRetry::default();
        let mut now = Instant::now();
        for _ in 0..16 {
            retry.failed(now);
            now += MAX_COOLDOWN;
        }
        assert_eq!(retry.cooldown(), MAX_COOLDOWN);
    }
}
