//! One wall-clock budget per read request, shared by every stage that can wait:
//! SQLite progress handlers, busy timeouts, publication waits, and member loops.
use std::time::{Duration, Instant};

/// Budget of one read request from accept to reply.
pub const QUERY_DEADLINE: Duration = Duration::from_secs(20);

/// Error prefix for work abandoned because its [`QueryDeadline`] expired.
pub const TIMED_OUT: &str = "timed_out";

/// Absolute expiry of one read request; `Copy` so each stage can hold its own.
#[derive(Clone, Copy, Debug)]
pub struct QueryDeadline {
    expires: Instant,
}

impl QueryDeadline {
    /// A deadline [`QUERY_DEADLINE`] from now.
    pub fn start() -> Self {
        Self::after(QUERY_DEADLINE)
    }

    pub fn after(budget: Duration) -> Self {
        Self {
            expires: Instant::now() + budget,
        }
    }

    pub fn expired(&self) -> bool {
        Instant::now() >= self.expires
    }

    pub fn remaining(&self) -> Duration {
        self.expires.saturating_duration_since(Instant::now())
    }

    /// Runs `work` off the query clock: the deadline moves later by the time it
    /// took. In-request reconciliation uses this so indexing never spends a
    /// query's budget; stores opened earlier keep their old deadline.
    pub fn pause_during<T>(&mut self, work: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let result = work();
        self.expires += started.elapsed();
        result
    }

    /// `wait`, shortened so it never outlives this deadline.
    pub fn cap(&self, wait: Duration) -> Duration {
        wait.min(self.remaining())
    }

    /// Relabels a SQLite interrupt raised by this deadline's progress handler as
    /// `timed_out:`; other errors pass through unchanged.
    pub fn classify(&self, error: anyhow::Error) -> anyhow::Error {
        let interrupted = error.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<rusqlite::Error>(),
                Some(rusqlite::Error::SqliteFailure(failure, _))
                    if failure.code == rusqlite::ErrorCode::OperationInterrupted
            )
        });
        if interrupted {
            error.context(format!("{TIMED_OUT}: query deadline expired"))
        } else {
            error
        }
    }
}

/// Whether `error` reports an expired query deadline.
pub fn is_timed_out(error: &anyhow::Error) -> bool {
    error
        .to_string()
        .strip_prefix(TIMED_OUT)
        .is_some_and(|rest| rest.starts_with(':'))
}

/// Writes enough Rust sources under `root` that reconciling them outlasts a
/// 100 ms query budget, plus one file defining `pub fn NAME`.
#[cfg(test)]
pub(crate) fn write_slow_reconcile_fixture(root: &std::path::Path, name: &str) {
    let mut body = String::with_capacity(64 * 48);
    for item in 0..64 {
        body.push_str(&format!(
            "pub fn filler_{item}(value: u8) -> u8 {{ value + {item} }}\n"
        ));
    }
    for file in 0..48 {
        std::fs::write(root.join(format!("filler_{file}.rs")), &body).unwrap();
    }
    std::fs::write(root.join("target.rs"), format!("pub fn {name}() {{}}\n")).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_deadline_caps_waits_to_zero() {
        let deadline = QueryDeadline::after(Duration::ZERO);
        assert!(deadline.expired());
        assert_eq!(deadline.remaining(), Duration::ZERO);
        assert_eq!(deadline.cap(Duration::from_secs(2)), Duration::ZERO);
        let fresh = QueryDeadline::start();
        assert!(!fresh.expired());
        assert_eq!(
            fresh.cap(Duration::from_millis(5)),
            Duration::from_millis(5)
        );
    }

    #[test]
    fn paused_work_does_not_spend_the_budget() {
        let mut deadline = QueryDeadline::after(Duration::from_millis(100));
        deadline.pause_during(|| std::thread::sleep(Duration::from_millis(200)));
        assert!(!deadline.expired());
        assert!(deadline.remaining() > Duration::from_millis(50));
    }

    #[test]
    fn classify_relabels_only_sqlite_interrupts() {
        let deadline = QueryDeadline::start();
        let interrupt = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_INTERRUPT),
            None,
        );
        let error = deadline.classify(anyhow::Error::new(interrupt).context("search"));
        assert!(is_timed_out(&error), "{error:#}");
        let other = deadline.classify(anyhow::anyhow!("stale_source: changed"));
        assert!(!is_timed_out(&other));
    }
}
