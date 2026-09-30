//! One client retry for read verbs whose failure is transient contention, not an
//! answer: a timed-out socket read, a busy daemon or SQLite lock, or an index
//! still warming. The retry carries a fresh request id.
use super::Arguments;
use crate::diagnostics::RequestContext;
use anyhow::Result;
use std::io::ErrorKind;
use std::time::Duration;

/// Pause before retrying `index_warming`; a small root's first scan fits in it.
const WARMING_RETRY_DELAY: Duration = Duration::from_secs(2);
/// Pause before retrying `daemon_busy` or `database is locked`.
const CONTENTION_RETRY_DELAY: Duration = Duration::from_millis(250);

/// Whether a failed command runs again, and after how long.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RetryPolicy {
    Retry(Duration),
    Fail,
}

impl RetryPolicy {
    /// Read verbs retry transient failures; everything else fails as is.
    pub(super) fn classify(verb: &str, error: &anyhow::Error) -> Self {
        if !matches!(
            verb,
            "search" | "refs" | "map" | "show" | "more" | "ctx" | "status"
        ) {
            return Self::Fail;
        }
        let io_timeout = error.chain().any(|cause| {
            cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
                matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
            })
        });
        // Relayed daemon failures arrive as text; `timed_out:` (the query
        // deadline) is an answer and never matches these.
        let message = format!("{error:#}");
        if message.contains("index_warming") {
            Self::Retry(WARMING_RETRY_DELAY)
        } else if message.contains(crate::daemon::DAEMON_BUSY)
            || message.contains("database is locked")
        {
            Self::Retry(CONTENTION_RETRY_DELAY)
        } else if io_timeout
            || message.contains("(os error 11)")
            || message.contains("(os error 110)")
        {
            Self::Retry(Duration::ZERO)
        } else {
            Self::Fail
        }
    }
}

/// Runs the command, retrying once when [`RetryPolicy::classify`] allows it.
pub(super) fn run_with_retry(
    args: &[String],
    options: &Arguments,
    context: &RequestContext,
) -> Result<String> {
    let verb = options.words.first().map_or("status", String::as_str);
    match super::run_with_context(args, context) {
        Err(error) => match RetryPolicy::classify(verb, &error) {
            RetryPolicy::Retry(delay) => {
                std::thread::sleep(delay);
                let fresh = RequestContext::new(context.session.clone(), context.client.clone());
                super::run_with_context(args, &fresh)
            }
            RetryPolicy::Fail => Err(error),
        },
        answered => answered,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn io(kind: ErrorKind) -> anyhow::Error {
        anyhow::Error::new(std::io::Error::from(kind)).context("read daemon frame length")
    }

    #[test]
    fn retry_policy_retries_transient_read_failures_once_classified() {
        let retried = [
            (io(ErrorKind::WouldBlock), Duration::ZERO),
            (io(ErrorKind::TimedOut), Duration::ZERO),
            (
                anyhow::anyhow!(
                    "daemon: read daemon frame length: Resource temporarily unavailable (os error 11)"
                ),
                Duration::ZERO,
            ),
            (
                anyhow::anyhow!("daemon: daemon: {}", crate::daemon::DAEMON_BUSY),
                CONTENTION_RETRY_DELAY,
            ),
            (
                anyhow::anyhow!("database is locked: Error code 5"),
                CONTENTION_RETRY_DELAY,
            ),
            (
                anyhow::anyhow!("daemon: index_warming: initial index is not published"),
                WARMING_RETRY_DELAY,
            ),
        ];
        for (error, delay) in &retried {
            for verb in ["search", "refs", "map", "show", "more", "ctx", "status"] {
                assert_eq!(
                    RetryPolicy::classify(verb, error),
                    RetryPolicy::Retry(*delay),
                    "{verb}: {error:#}"
                );
            }
        }
        for verb in ["index", "session", "forget-logs", "stop", "semantic"] {
            assert_eq!(
                RetryPolicy::classify(verb, &retried[0].0),
                RetryPolicy::Fail
            );
        }
        for answer in [
            "timed_out: query deadline expired",
            "stale_source: source revision changed",
            "no_definition: no indexed definition named `x`",
            "invalid_command: unknown command next",
        ] {
            assert_eq!(
                RetryPolicy::classify("search", &anyhow::anyhow!("daemon: {answer}")),
                RetryPolicy::Fail,
                "{answer}"
            );
        }
    }
}
