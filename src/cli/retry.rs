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
                    "show" | "review" | "ingest" | "feed" | "attention" | "history" | "search"
                    | "web",
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
    let is_web = options.words.first().map(String::as_str) == Some("board")
        && options.words.get(1).is_some_and(|word| word == "web");
    let args = if options.is_board() && !is_web {
        let parsed = super::parse(args)?;
        prepared = crate::board::prepare_client(args, &parsed)?;
        prepared.as_slice()
    } else {
        args
    };
    let scope = RetryScope::for_options(options);
    let started = Instant::now();
    let mut command = super::ClientCommand::default();
    match command.run(args, context) {
        Err(error) => match RetryPolicy::classify(scope, &error, started.elapsed()) {
            RetryPolicy::Retry(delay) => {
                std::thread::sleep(delay);
                let fresh = RequestContext::new(context.session.clone(), context.client.clone());
                command.run(args, &fresh)
            }
            RetryPolicy::Fail => Err(error),
        },
        answered => answered,
    }
}

#[cfg(test)]
mod tests;
