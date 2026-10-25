use std::fmt::Write as _;

const RETRY_AFTER_MS: u16 = 250;
const DIAGNOSTIC_CAPACITY: usize = 400;
const LABEL_LIMIT: usize = 48;

/// Describes why admission refused a request before dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RefusalCause {
    QueueFull,
    PoolClosed,
    FdExhausted,
    StreamCloneFailed,
    RootLimit,
}

impl RefusalCause {
    fn token(self) -> &'static str {
        match self {
            Self::QueueFull => "queue_full",
            Self::PoolClosed => "pool_closed",
            Self::FdExhausted => "fd_exhausted",
            Self::StreamCloneFailed => "stream_clone_failed",
            Self::RootLimit => "root_limit",
        }
    }
}

/// A bounded, source-free diagnostic for a request refused before dispatch.
pub(crate) struct AdmissionRefusal<'a> {
    pool: &'a str,
    queue_depth: usize,
    queue_capacity: usize,
    cause: RefusalCause,
    active_verb: Option<&'a str>,
    root_limit: Option<(usize, usize)>,
}

impl<'a> AdmissionRefusal<'a> {
    /// Creates a refusal from a controlled pool label and its queue snapshot.
    pub(crate) fn new(
        pool: &'a str,
        queue_depth: usize,
        queue_capacity: usize,
        cause: RefusalCause,
    ) -> Self {
        Self {
            pool,
            queue_depth,
            queue_capacity,
            cause,
            active_verb: None,
            root_limit: None,
        }
    }

    /// Classifies descriptor exhaustion without retaining the error message.
    pub(crate) fn clone_failed(
        pool: &'a str,
        queue_depth: usize,
        queue_capacity: usize,
        error: &std::io::Error,
    ) -> Self {
        let cause = match error.raw_os_error() {
            Some(code) if code == libc::EMFILE || code == libc::ENFILE => RefusalCause::FdExhausted,
            _ => RefusalCause::StreamCloneFailed,
        };
        Self::new(pool, queue_depth, queue_capacity, cause)
    }

    /// Adds a fixed classification label, never request or source contents.
    pub(crate) fn with_active_verb(mut self, active_verb: Option<&'a str>) -> Self {
        self.active_verb = active_verb;
        self
    }

    /// Adds bounded outstanding-work counts without naming a protected root.
    pub(crate) fn with_root_limit(mut self, outstanding: usize, limit: usize) -> Self {
        self.root_limit = Some((outstanding, limit));
        self
    }

    /// The refusal as a typed `daemon_busy` error.
    pub(crate) fn error(&self) -> tp_core::coded_error::CodedError {
        coded!(DaemonBusy, "{}", self.detail())
    }

    /// Returns the wire diagnostic with bounded ASCII-only labels.
    pub(crate) fn message(&self) -> String {
        self.error().to_string()
    }

    fn detail(&self) -> String {
        let mut message = String::with_capacity(DIAGNOSTIC_CAPACITY);
        let _ = write!(
            message,
            "pool={} queue_depth={} queue_capacity={} cause={} retry_after_ms={} hint=retry_with_bounded_backoff",
            safe_label(self.pool),
            self.queue_depth,
            self.queue_capacity,
            self.cause.token(),
            RETRY_AFTER_MS,
        );
        if let Some(active_verb) = self.active_verb.and_then(safe_optional_label) {
            let _ = write!(message, " active_verb={active_verb}");
        }
        if let Some((outstanding, limit)) = self.root_limit {
            let _ = write!(
                message,
                " root_outstanding={outstanding} root_capacity={limit}"
            );
        }
        message
    }

    /// Logs the exact diagnostic text included in a refusal reply.
    pub(crate) fn log(&self) {
        eprintln!("trufflepig: {}", self.message());
    }
}

fn safe_label(label: &str) -> &str {
    if is_safe_label(label) {
        label
    } else {
        "unknown"
    }
}

fn safe_optional_label(label: &str) -> Option<&str> {
    is_safe_label(label).then_some(label)
}

fn is_safe_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= LABEL_LIMIT
        && label
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte))
}
