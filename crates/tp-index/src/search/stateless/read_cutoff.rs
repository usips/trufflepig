//! The read window of one cache-free scan. Walking and reading stop at a cutoff
//! that keeps a reserve of the query deadline for page assembly and handle
//! verification, so an expiring deadline limits coverage instead of failing.
use std::time::Duration;
use tp_core::query_deadline::QueryDeadline;

/// The reserve is this fraction (1/N) of the deadline remaining when reading starts.
const ASSEMBLY_RESERVE_DIVISOR: u32 = 4;
/// Files at least this large start extraction only while half the window remains,
/// because one extraction cannot be interrupted.
pub(super) const LARGE_EXTRACTION_BYTES: usize = 256 * 1024;

/// When reading must stop; once reached it stays reached.
pub(super) struct ReadCutoff {
    window_end: QueryDeadline,
    window: Duration,
    reads_left: Option<usize>,
    reached: bool,
}

impl ReadCutoff {
    pub(super) fn for_deadline(deadline: &QueryDeadline) -> Self {
        let remaining = deadline.remaining();
        let window = remaining.saturating_sub(remaining / ASSEMBLY_RESERVE_DIVISOR);
        Self {
            window_end: deadline.capped(window),
            window,
            reads_left: None,
            reached: false,
        }
    }

    /// A cutoff that is also reached after `reads` file reads: a deterministic
    /// stand-in for a deadline that expires mid-scan.
    #[cfg(test)]
    pub(super) fn after_reads(deadline: &QueryDeadline, reads: usize) -> Self {
        Self {
            reads_left: Some(reads),
            ..Self::for_deadline(deadline)
        }
    }

    pub(super) fn reached(&mut self) -> bool {
        self.reached |= self.reads_left == Some(0) || self.window_end.expired();
        self.reached
    }

    pub(super) fn record_read(&mut self) {
        if let Some(left) = &mut self.reads_left {
            *left = left.saturating_sub(1);
        }
    }

    /// Whether an extraction of `bytes` may start without risking the reserve.
    pub(super) fn admits_extraction(&self, bytes: usize) -> bool {
        bytes < LARGE_EXTRACTION_BYTES || self.window_end.remaining() * 2 > self.window
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cutoff_keeps_a_reserve_and_stays_reached() {
        let deadline = QueryDeadline::after(Duration::from_secs(20));
        let cutoff = ReadCutoff::for_deadline(&deadline);
        assert!(cutoff.window <= Duration::from_secs(15));
        assert!(cutoff.window >= Duration::from_secs(14));

        let mut counted = ReadCutoff::after_reads(&deadline, 2);
        assert!(!counted.reached());
        counted.record_read();
        counted.record_read();
        assert!(counted.reached());
        assert!(counted.reached());

        let mut expired = ReadCutoff::for_deadline(&QueryDeadline::after(Duration::ZERO));
        assert!(expired.reached());
        assert!(!expired.admits_extraction(LARGE_EXTRACTION_BYTES));
        assert!(expired.admits_extraction(LARGE_EXTRACTION_BYTES - 1));
    }
}
