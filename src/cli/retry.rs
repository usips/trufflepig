//! One retry for transient failures, restricted by the operation’s write safety.
//! Board writes retry only explicit pre-dispatch contention; a lost reply may
//! already represent a committed transaction. Each retry has a fresh request id.
use super::Arguments;
use crate::board::board_protocol::{BoardErrorCode, leading_error_code};
use crate::diagnostics::RequestContext;
use anyhow::Result;
use std::io::ErrorKind;
use std::time::{Duration, Instant};

/// Pause before retrying `index_warming`; a small root's first scan fits in it.
const WARMING_RETRY_DELAY: Duration = Duration::from_secs(2);
/// Pause before retrying `daemon_busy`, `database is locked`, or a dropped connection.
const CONTENTION_RETRY_DELAY: Duration = Duration::from_millis(250);
/// A socket timeout that took longer than this was a full reply wait; only
/// quicker transport failures are retried.
const FAST_TRANSPORT_FAILURE: Duration = Duration::from_secs(5);

/// Transport failures are replayable only when the operation cannot mutate state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RetryScope {
    Read,
    ContentionOnly,
    Never,
}

impl RetryScope {
    pub(super) fn for_options(options: &Arguments) -> Self {
        let verb = options.words.first().map_or("status", String::as_str);
        match verb {
            "board" => match options.words.get(1).map(String::as_str) {
                Some(
                    "show" | "review" | "ingest" | "feed" | "attention" | "history" | "search",
                ) => Self::Read,
                Some("inbox") if options.words.get(2).is_some() => Self::Read,
                Some(word) if word.parse::<u64>().is_ok() => Self::Read,
                _ => Self::ContentionOnly,
            },
            "feedback" if options.words.get(1).is_some_and(|word| word == "ls") => Self::Read,
            "feedback" => Self::ContentionOnly,
            "search" | "refs" | "map" | "show" | "more" | "ctx" | "status" => Self::Read,
            _ => Self::Never,
        }
    }
}

/// Whether a failed command runs again, and after how long.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RetryPolicy {
    Retry(Duration),
    Fail,
}

impl RetryPolicy {
    /// Restricts transient retries to failures safe for this operation.
    /// `elapsed` is how long the failed attempt took.
    pub(super) fn classify(scope: RetryScope, error: &anyhow::Error, elapsed: Duration) -> Self {
        if scope == RetryScope::Never {
            return Self::Fail;
        }
        let message = format!("{error:#}");
        if matches!(
            BoardErrorCode::from_error(error),
            Some(BoardErrorCode::DaemonBusy | BoardErrorCode::DatabaseLocked)
        ) {
            return Self::Retry(CONTENTION_RETRY_DELAY);
        }
        if scope == RetryScope::ContentionOnly {
            return Self::Fail;
        }
        let io_kind = |kinds: &[ErrorKind]| {
            error.chain().any(|cause| {
                cause
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| kinds.contains(&error.kind()))
            })
        };
        // Relayed daemon failures arrive as text; `timed_out:` (the query
        // deadline) is an answer and never matches these.
        let dropped = io_kind(&[ErrorKind::BrokenPipe, ErrorKind::ConnectionReset])
            || message.contains("(os error 32)")
            || message.contains("(os error 104)");
        let timed_out = io_kind(&[ErrorKind::WouldBlock, ErrorKind::TimedOut])
            || message.contains("(os error 11)")
            || message.contains("(os error 110)");
        if error.chain().any(|cause| {
            leading_error_code(&cause.to_string()).is_some_and(|(code, _)| code == "index_warming")
        }) {
            Self::Retry(WARMING_RETRY_DELAY)
        } else if dropped {
            Self::Retry(CONTENTION_RETRY_DELAY)
        } else if timed_out && elapsed < FAST_TRANSPORT_FAILURE {
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
    // Capture client-owned body/stdin once. Replaying normalized arguments keeps
    // content and feedback import identity unchanged after explicit contention.
    let prepared;
    let args = if options.is_board() {
        let parsed = super::parse(args)?;
        prepared = crate::board::prepare_client(args, &parsed)?;
        prepared.as_slice()
    } else {
        args
    };
    let scope = RetryScope::for_options(options);
    let started = Instant::now();
    match super::run_with_context(args, context) {
        Err(error) => match RetryPolicy::classify(scope, &error, started.elapsed()) {
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

    const QUICK: Duration = Duration::from_millis(50);

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
            (io(ErrorKind::BrokenPipe), CONTENTION_RETRY_DELAY),
            (
                anyhow::anyhow!("daemon: Connection reset by peer (os error 104)"),
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
                    RetryPolicy::classify(RetryScope::Read, error, QUICK),
                    RetryPolicy::Retry(*delay),
                    "{verb}: {error:#}"
                );
            }
        }
        for _ in ["index", "session", "forget-logs", "stop", "semantic"] {
            assert_eq!(
                RetryPolicy::classify(RetryScope::Never, &retried[0].0, QUICK),
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
                RetryPolicy::classify(
                    RetryScope::Read,
                    &anyhow::anyhow!("daemon: {answer}"),
                    QUICK
                ),
                RetryPolicy::Fail,
                "{answer}"
            );
        }
    }

    #[test]
    fn board_writes_do_not_replay_ambiguous_transport_failures() {
        for error in [
            io(ErrorKind::BrokenPipe),
            io(ErrorKind::TimedOut),
            anyhow::anyhow!("index_warming: unavailable"),
        ] {
            assert_eq!(
                RetryPolicy::classify(RetryScope::ContentionOnly, &error, QUICK),
                RetryPolicy::Fail
            );
        }
        for message in [crate::daemon::DAEMON_BUSY, "database is locked"] {
            assert_eq!(
                RetryPolicy::classify(RetryScope::ContentionOnly, &anyhow::anyhow!(message), QUICK),
                RetryPolicy::Retry(CONTENTION_RETRY_DELAY)
            );
        }
    }

    #[test]
    fn busy_replies_retry_by_code_independent_of_explanation() {
        for message in [
            "daemon_busy: isolated injected queue refusal",
            "daemon: daemon_busy: worker pool is full",
            "daemon: daemon: daemon_busy: try another request later",
        ] {
            let error = anyhow::anyhow!(message).context("route request through system router");
            for scope in [RetryScope::Read, RetryScope::ContentionOnly] {
                assert_eq!(
                    RetryPolicy::classify(scope, &error, crate::daemon::PROXY_REPLY_WAIT),
                    RetryPolicy::Retry(CONTENTION_RETRY_DELAY),
                    "{message}"
                );
            }
            assert_eq!(
                RetryPolicy::classify(RetryScope::Never, &error, QUICK),
                RetryPolicy::Fail
            );
        }
    }

    #[test]
    fn diagnostic_mentions_of_busy_do_not_replay_writes() {
        for message in [
            "invalid_reference: body mentions daemon_busy: retry",
            "daemon: invalid_body: daemon_busy: isolated injected queue refusal",
            "read daemon frame: broken pipe; possible daemon_busy: retry",
            "daemon_busy_note: this is not a busy reply",
        ] {
            assert_eq!(
                RetryPolicy::classify(RetryScope::ContentionOnly, &anyhow::anyhow!(message), QUICK),
                RetryPolicy::Fail,
                "{message}"
            );
        }
    }

    #[test]
    fn diagnostic_lock_mentions_never_replay_board_writes() {
        for message in [
            "invalid_body: submitted text says database is locked",
            "daemon: invalid_options: database is locked: quoted diagnostic",
            "read reply: database is locked might explain it",
        ] {
            assert_eq!(
                RetryPolicy::classify(RetryScope::ContentionOnly, &anyhow::anyhow!(message), QUICK),
                RetryPolicy::Fail,
                "{message}"
            );
        }
        let sqlite = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("custom sqlite contention message".into()),
        );
        assert_eq!(
            RetryPolicy::classify(
                RetryScope::ContentionOnly,
                &anyhow::Error::new(sqlite).context("write board"),
                QUICK,
            ),
            RetryPolicy::Retry(CONTENTION_RETRY_DELAY),
        );
    }

    #[test]
    fn board_scope_preserves_cursor_and_write_safety() {
        let options = |words: &[&str]| {
            super::super::parse(
                &words
                    .iter()
                    .map(|word| (*word).to_owned())
                    .collect::<Vec<_>>(),
            )
            .unwrap()
        };
        for words in [
            vec!["board", "show", "P1"],
            vec!["board", "review", "P1@1"],
            vec!["board", "ingest"],
            vec!["board", "inbox", "8"],
            vec!["feedback", "ls"],
        ] {
            assert_eq!(RetryScope::for_options(&options(&words)), RetryScope::Read);
        }
        for words in [
            vec!["board"],
            vec!["board", "inbox"],
            vec!["board", "post", "P1", "note", "body"],
            vec!["feedback", "blocked", "summary"],
        ] {
            assert_eq!(
                RetryScope::for_options(&options(&words)),
                RetryScope::ContentionOnly
            );
        }
    }

    #[test]
    fn retry_policy_never_retries_a_full_reply_wait() {
        let waited = crate::daemon::PROXY_REPLY_WAIT;
        for error in [
            io(ErrorKind::WouldBlock),
            anyhow::anyhow!("daemon: Resource temporarily unavailable (os error 11)"),
        ] {
            assert_eq!(
                RetryPolicy::classify(RetryScope::Read, &error, waited),
                RetryPolicy::Fail
            );
        }
        // An explicit busy reply is retried however long it took to arrive.
        let busy = anyhow::anyhow!("daemon: {}", crate::daemon::DAEMON_BUSY);
        assert_eq!(
            RetryPolicy::classify(RetryScope::Read, &busy, waited),
            RetryPolicy::Retry(CONTENTION_RETRY_DELAY)
        );
    }
}
