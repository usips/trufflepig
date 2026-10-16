//! One wall-clock budget per read request, shared by every stage that can wait:
//! SQLite progress handlers, busy timeouts, publication waits, and member loops.
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Local execution budget for a bounded query.
pub const QUERY_DEADLINE: Duration = Duration::from_secs(20);

/// Error prefix for work abandoned because its [`QueryDeadline`] expired.
pub const TIMED_OUT: &str = "timed_out";

/// Absolute expiry and optional peer or spool-claim cancellation signals.
#[derive(Clone, Debug)]
pub struct QueryDeadline {
    expires: Instant,
    peer: Option<Arc<UnixStream>>,
    claim_path: Option<Arc<PathBuf>>,
}

impl QueryDeadline {
    /// A deadline [`QUERY_DEADLINE`] from now.
    pub fn start() -> Self {
        Self::after(QUERY_DEADLINE)
    }

    pub fn after(budget: Duration) -> Self {
        Self {
            expires: Instant::now() + budget,
            peer: None,
            claim_path: None,
        }
    }

    pub fn expired(&self) -> bool {
        self.time_expired() || self.cancelled()
    }

    /// Checks peer disconnection and claim removal without checking wall-clock expiry.
    pub fn cancelled(&self) -> bool {
        self.peer.as_ref().is_some_and(|peer| peer_closed(peer))
            || self
                .claim_path
                .as_ref()
                .is_some_and(|path| claim_was_removed(path))
    }

    /// Checks only the absolute wall-clock expiry, without polling cancellation sources.
    pub fn time_expired(&self) -> bool {
        Instant::now() >= self.expires
    }

    pub fn remaining(&self) -> Duration {
        if self.expired() {
            Duration::ZERO
        } else {
            self.expires.saturating_duration_since(Instant::now())
        }
    }

    /// Tracks the connected peer without owning a borrowed file descriptor.
    pub fn with_peer(mut self, peer: Arc<UnixStream>) -> Self {
        self.peer = Some(peer);
        self
    }

    /// Tracks a spool claim; removing the claimed file cancels this request.
    pub fn with_claim_path(mut self, path: PathBuf) -> Self {
        self.claim_path = Some(Arc::new(path));
        self
    }

    /// `wait`, shortened so it never outlives this deadline.
    pub fn cap(&self, wait: Duration) -> Duration {
        wait.min(self.remaining())
    }

    /// Keeps this caller's expiry, capped by `maximum` from now.
    pub fn capped(&self, maximum: Duration) -> Self {
        let cap = Instant::now() + maximum;
        Self {
            expires: self.expires.min(cap),
            peer: self.peer.clone(),
            claim_path: self.claim_path.clone(),
        }
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

/// Checks for a disconnected peer without consuming request or protocol bytes.
fn peer_closed(peer: &UnixStream) -> bool {
    let mut descriptor = libc::pollfd {
        fd: peer.as_raw_fd(),
        events: 0,
        revents: 0,
    };
    // SAFETY: `peer` owns the descriptor for this call, `descriptor` points to
    // one initialized pollfd, and a zero timeout makes this check nonblocking.
    let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
    result > 0 && descriptor.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
}

/// Only an explicitly removed spool claim is a cancellation signal. A
/// transient metadata failure keeps the request alive until its age deadline.
fn claim_was_removed(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
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
        assert!(deadline.time_expired());
        assert!(!deadline.cancelled());
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
    fn peer_drop_expires_and_zeroes_remaining_budget() {
        let (daemon, client) = UnixStream::pair().unwrap();
        let deadline = QueryDeadline::after(Duration::from_secs(5)).with_peer(Arc::new(daemon));
        assert!(!deadline.expired());
        assert!(!deadline.cancelled());
        drop(client);
        assert!(deadline.expired());
        assert!(deadline.cancelled());
        assert_eq!(deadline.remaining(), Duration::ZERO);
    }

    #[test]
    fn wall_clock_check_defers_peer_cancellation_polling() {
        let (daemon, client) = UnixStream::pair().unwrap();
        let deadline = QueryDeadline::after(Duration::from_secs(5)).with_peer(Arc::new(daemon));
        drop(client);

        assert!(!deadline.time_expired());
        assert!(deadline.cancelled());
        assert!(deadline.expired());

        let combined =
            QueryDeadline::after(Duration::ZERO).with_peer(deadline.peer.as_ref().unwrap().clone());
        assert!(combined.time_expired());
        assert!(combined.cancelled());
        assert!(combined.expired());
    }

    #[test]
    fn peer_probe_does_not_consume_pending_bytes() {
        use std::io::{Read, Write};

        let (daemon, mut client) = UnixStream::pair().unwrap();
        let mut daemon_reader = daemon.try_clone().unwrap();
        let deadline = QueryDeadline::after(Duration::from_secs(5)).with_peer(Arc::new(daemon));
        client.write_all(b"request").unwrap();
        assert!(!deadline.expired());
        let mut bytes = [0; 7];
        daemon_reader.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"request");
    }

    #[test]
    fn write_half_close_does_not_cancel_a_waiting_reader() {
        use std::io::{Read, Write};
        use std::net::Shutdown;

        let (daemon, mut client) = UnixStream::pair().unwrap();
        let deadline = QueryDeadline::after(Duration::from_secs(5)).with_peer(Arc::new(daemon));
        client.shutdown(Shutdown::Write).unwrap();
        assert!(!deadline.expired());
        let mut daemon_writer = deadline.peer.as_ref().unwrap().as_ref();
        daemon_writer.write_all(b"reply").unwrap();
        let mut reply = [0; 5];
        client.read_exact(&mut reply).unwrap();
        assert_eq!(&reply, b"reply");
    }

    #[test]
    fn capping_a_deadline_never_extends_its_expiry() {
        let expired = QueryDeadline::after(Duration::ZERO).capped(Duration::from_secs(10));
        assert!(expired.expired());

        let deadline = QueryDeadline::start().capped(Duration::from_millis(5));
        assert!(deadline.remaining() <= Duration::from_millis(5));
    }

    #[test]
    fn capped_clone_keeps_peer_cancellation() {
        let (daemon, client) = UnixStream::pair().unwrap();
        let deadline = QueryDeadline::after(Duration::from_secs(5))
            .with_peer(Arc::new(daemon))
            .capped(Duration::from_secs(4));
        let clone = deadline.clone();
        drop(client);
        assert!(deadline.expired());
        assert!(deadline.cancelled());
        assert!(clone.expired());
        assert!(clone.cancelled());
    }

    #[test]
    fn capped_clone_keeps_claim_cancellation() {
        let executable = std::env::current_exe().unwrap();
        let present = QueryDeadline::after(Duration::from_secs(5))
            .with_claim_path(executable.clone())
            .capped(Duration::from_secs(4));
        assert!(!present.expired());
        assert!(!present.cancelled());

        let missing = executable.with_extension(format!(
            "removed-claim-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let deadline = QueryDeadline::after(Duration::from_secs(5))
            .with_claim_path(missing)
            .capped(Duration::from_secs(4));
        let clone = deadline.clone();
        assert!(deadline.expired());
        assert!(deadline.cancelled());
        assert!(clone.expired());
        assert!(clone.cancelled());
    }

    #[test]
    fn non_not_found_claim_metadata_errors_do_not_cancel() {
        let not_a_directory = std::env::current_exe().unwrap().join("claim");
        let deadline =
            QueryDeadline::after(Duration::from_secs(5)).with_claim_path(not_a_directory);
        assert!(!deadline.expired());
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
