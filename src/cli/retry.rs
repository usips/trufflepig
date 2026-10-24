//! Bounded retries for transient failures, restricted by operation safety.
use super::Arguments;
use crate::board::board_protocol::BoardErrorCode;
use crate::coded_error::{ErrorCode, coded_error};
use crate::request_context::RequestContext;
use crate::{daemon_transport::DaemonReplyTimeout, query_deadline::QueryDeadline};
use anyhow::Result;
use std::io::ErrorKind;
use std::time::{Duration, Instant};

const READ_ATTEMPTS: usize = 5;
const CONTENTION_ATTEMPTS: usize = 2;
const CONTENTION_RETRY_DELAY: Duration = Duration::from_millis(250);
const WARMING_RETRY_DELAY: Duration = Duration::from_secs(2);
const MAX_RETRY_BACKOFF: Duration = Duration::from_secs(2);
const USEFUL_ATTEMPT_FLOOR: Duration = Duration::from_secs(1);
const BUSY_FALLBACK_RESERVE: Duration = Duration::from_millis(250);
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
                    "show" | "review" | "feed" | "attention" | "history" | "search" | "web"
                    | "pending" | "receipt",
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

    fn max_attempts(self) -> usize {
        match self {
            Self::Read => READ_ATTEMPTS,
            Self::ContentionOnly => CONTENTION_ATTEMPTS,
            Self::Never => 1,
        }
    }
}

/// Whether a failed command is safe to retry and its minimum retry delay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RetryPolicy {
    Retry(Duration),
    Fail,
}

impl RetryPolicy {
    /// Restricts transient retries to failures safe for this operation.
    /// `elapsed` is how long the failed attempt took.
    pub(super) fn classify(scope: RetryScope, error: &anyhow::Error, elapsed: Duration) -> Self {
        if scope == RetryScope::Never || ErrorCode::TimedOut.is_code_of(error) {
            return Self::Fail;
        }
        if ErrorCode::DaemonBusy.is_code_of(error)
            || BoardErrorCode::from_error(error) == Some(BoardErrorCode::DatabaseLocked)
        {
            return Self::Retry(CONTENTION_RETRY_DELAY);
        }
        if scope == RetryScope::ContentionOnly {
            return Self::Fail;
        }
        if let Some(timeout) = DaemonReplyTimeout::from_error(error) {
            return Self::Retry(timeout.retry_after());
        }

        let message = format!("{error:#}");
        let io_kind = |kinds: &[ErrorKind]| {
            error.chain().any(|cause| {
                cause
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| kinds.contains(&error.kind()))
            })
        };
        // Only typed transport timeouts retry; stringified timeout answers do not.
        let dropped = io_kind(&[ErrorKind::BrokenPipe, ErrorKind::ConnectionReset])
            || message.contains("(os error 32)")
            || message.contains("(os error 104)");
        let timed_out = io_kind(&[ErrorKind::WouldBlock, ErrorKind::TimedOut]);
        if ErrorCode::IndexWarming.is_code_of(error) {
            Self::Retry(WARMING_RETRY_DELAY)
        } else if dropped {
            Self::Retry(CONTENTION_RETRY_DELAY)
        } else if timed_out && elapsed < FAST_TRANSPORT_FAILURE {
            Self::Retry(CONTENTION_RETRY_DELAY)
        } else {
            Self::Fail
        }
    }
}

enum RetryInvocation {
    /// `held_back` is budget the attempt must leave for the busy fallback.
    Attempt {
        index: usize,
        held_back: Duration,
    },
    BusyFallback,
}

enum RetryOutput {
    Attempt(Result<String>, Duration),
    BusyFallback(Option<Result<String>>),
}

/// Runs an injected retry loop so its attempts, clock, waits, and fallback are testable.
/// `fallback_available` is whether a read has a local busy fallback to hold time for.
fn run_retry_loop(
    scope: RetryScope,
    request_id: &str,
    fallback_available: bool,
    mut remaining: impl FnMut() -> Duration,
    mut wait: impl FnMut(Duration),
    mut invoke: impl FnMut(RetryInvocation) -> RetryOutput,
) -> Result<String> {
    let max_attempts = scope.max_attempts();
    let holds_fallback_time = scope == RetryScope::Read && fallback_available;
    let mut attempts = 0;
    loop {
        let held_back = if holds_fallback_time {
            BUSY_FALLBACK_RESERVE
        } else {
            Duration::ZERO
        };
        let RetryOutput::Attempt(result, elapsed) = invoke(RetryInvocation::Attempt {
            index: attempts,
            held_back,
        }) else {
            unreachable!("retry loop invokes an attempt before its fallback");
        };
        attempts += 1;
        match result {
            Ok(answer) => return Ok(answer),
            Err(error) => {
                let policy = RetryPolicy::classify(scope, &error, elapsed);
                if let RetryPolicy::Retry(minimum_delay) = policy
                    && attempts < max_attempts
                {
                    let available = remaining();
                    let delay = retry_delay(request_id, attempts - 1, &error).max(minimum_delay);
                    let reserve = retry_reserve(holds_fallback_time);
                    // Preserve enough time to make the next request useful and,
                    // after a busy refusal, to fall back locally.
                    if delay.saturating_add(reserve) < available {
                        wait(delay);
                        // A scheduler delay can oversleep. The prior busy reply
                        // remains valid only until a next request actually starts.
                        if remaining() > reserve {
                            continue;
                        }
                    }
                }

                // A timeout counts only when this attempt's own capped deadline
                // expired; a timeout relayed earlier keeps the error as the answer.
                let capped_timeout = held_back > Duration::ZERO
                    && is_timeout_answer(&error)
                    && remaining() <= held_back;
                if scope == RetryScope::Read
                    && (is_explicit_busy(&error) || capped_timeout)
                    && !remaining().is_zero()
                    && let RetryOutput::BusyFallback(Some(answer)) =
                        invoke(RetryInvocation::BusyFallback)
                {
                    return match answer {
                        // The original reply still names the failure; the fallback
                        // only explains why the local read could not finish.
                        Err(fallback) if ErrorCode::TimedOut.is_code_of(&fallback) => {
                            Err(error.context(format!("local fallback failed: {fallback:#}")))
                        }
                        answer => answer,
                    };
                }
                return Err(error);
            }
        }
    }
}

/// Whether a failed attempt is a typed reply timeout or a `timed_out` answer.
fn is_timeout_answer(error: &anyhow::Error) -> bool {
    DaemonReplyTimeout::from_error(error).is_some() || ErrorCode::TimedOut.is_code_of(error)
}

/// One attempt's deadline: the shared read budget less the time held back for the fallback.
fn attempt_deadline(read_deadline: &QueryDeadline, held_back: Duration) -> QueryDeadline {
    read_deadline.capped(read_deadline.remaining().saturating_sub(held_back))
}

fn retry_reserve(holds_fallback_time: bool) -> Duration {
    USEFUL_ATTEMPT_FLOOR
        + if holds_fallback_time {
            BUSY_FALLBACK_RESERVE
        } else {
            Duration::ZERO
        }
}

fn backoff(request_id: &str, retry_index: usize) -> Duration {
    let step = 1_u64 << retry_index.min(3);
    let base_ms = (CONTENTION_RETRY_DELAY.as_millis() as u64 * step)
        .min(MAX_RETRY_BACKOFF.as_millis() as u64);
    let lower_ms = (base_ms - base_ms / 4).max(CONTENTION_RETRY_DELAY.as_millis() as u64);
    let upper_ms = (base_ms + base_ms / 4).min(MAX_RETRY_BACKOFF.as_millis() as u64);
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in request_id.bytes().chain((retry_index as u64).to_le_bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let span = upper_ms - lower_ms + 1;
    Duration::from_millis(lower_ms + hash % span)
}

fn is_explicit_busy(error: &anyhow::Error) -> bool {
    ErrorCode::DaemonBusy.is_code_of(error)
}

/// Reads the `retry_after_ms=` field of a `daemon_busy` refusal.
fn busy_retry_after(error: &anyhow::Error) -> Option<Duration> {
    let busy = coded_error(error).filter(|coded| coded.code() == ErrorCode::DaemonBusy)?;
    busy.message()
        .split_ascii_whitespace()
        .find_map(|token| token.strip_prefix("retry_after_ms=")?.parse::<u64>().ok())
        .map(Duration::from_millis)
}

fn retry_delay(request_id: &str, retry_index: usize, error: &anyhow::Error) -> Duration {
    let exponential = backoff(request_id, retry_index);
    busy_retry_after(error)
        .or_else(|| DaemonReplyTimeout::from_error(error).map(|timeout| timeout.retry_after()))
        .map_or(exponential, |hint| {
            exponential.max(hint.min(MAX_RETRY_BACKOFF))
        })
}

/// Runs the command with one absolute read budget and a fresh write-attempt wait.
pub(super) fn run_with_retry(
    args: &[String],
    options: &Arguments,
    context: &RequestContext,
) -> Result<String> {
    let read_deadline = (RetryScope::for_options(options) == RetryScope::Read)
        .then(|| QueryDeadline::after(crate::daemon_transport::CLIENT_REPLY_WAIT));
    run_with_retry_deadline(args, options, context, read_deadline)
}

fn run_with_retry_deadline(
    args: &[String],
    options: &Arguments,
    context: &RequestContext,
    read_deadline: Option<QueryDeadline>,
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
    let fallback_available = super::client_command::has_busy_fallback(args);
    if scope == RetryScope::Read
        && fallback_available
        && read_deadline.is_some()
        && !options.no_daemon
    {
        // Build the tokenizer beside the attempts, so an immediately answered read
        // does not pay for it. A fallback that needs it waits on the same one-time init.
        std::thread::spawn(crate::output::prewarm_tokenizer);
    }
    let mut command = super::ClientCommand::default();
    run_retry_loop(
        scope,
        &context.request_id,
        fallback_available,
        || {
            read_deadline
                .as_ref()
                .map_or(Duration::MAX, QueryDeadline::remaining)
        },
        std::thread::sleep,
        |invocation| match invocation {
            RetryInvocation::Attempt { index, held_back } => {
                let attempt_context = if index == 0 {
                    context.clone()
                } else {
                    context.next_attempt()
                };
                let deadline = read_deadline.as_ref().map_or_else(
                    || QueryDeadline::after(super::CLIENT_REPLY_WAIT),
                    |read_deadline| attempt_deadline(read_deadline, held_back),
                );
                let started = Instant::now();
                RetryOutput::Attempt(
                    command.run(args, &attempt_context, deadline),
                    started.elapsed(),
                )
            }
            RetryInvocation::BusyFallback => {
                let Some(deadline) = read_deadline.clone() else {
                    unreachable!("write scopes never use read fallback");
                };
                let fallback_context = context.next_attempt();
                RetryOutput::BusyFallback(command.run_after_busy_exhaustion(
                    args,
                    &fallback_context,
                    deadline,
                ))
            }
        },
    )
}

#[cfg(test)]
mod tests;
