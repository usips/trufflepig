use super::*;
use tp_core::coded_error::{CodedContext, CodedError};

mod retry_loop_tests;

const QUICK: Duration = Duration::from_millis(50);

fn io(kind: ErrorKind) -> anyhow::Error {
    anyhow::Error::new(std::io::Error::from(kind)).context("read daemon frame length")
}

fn terminal_io_timeout(kind: ErrorKind, phase: &str) -> anyhow::Error {
    let message = match phase {
        "connecting" => "daemon connect deadline expired",
        _ => "daemon request write deadline expired",
    };
    anyhow::Error::new(std::io::Error::new(kind, message))
        .with_coded_context(|| coded!(TimedOut, "phase={phase}"))
}

/// Types reply text as the daemon client boundary does.
fn relayed(text: &str) -> anyhow::Error {
    tp_core::coded_error::relayed_error(text.to_owned())
}

fn busy() -> anyhow::Error {
    relayed("daemon: daemon_busy: worker pool is full")
}

#[test]
fn retry_policy_classifies_only_safe_transient_read_failures() {
    let retry = [
        (io(ErrorKind::WouldBlock), CONTENTION_RETRY_DELAY),
        (io(ErrorKind::TimedOut), CONTENTION_RETRY_DELAY),
        (busy(), CONTENTION_RETRY_DELAY),
        (
            relayed("database is locked: Error code 5"),
            CONTENTION_RETRY_DELAY,
        ),
        (io(ErrorKind::BrokenPipe), CONTENTION_RETRY_DELAY),
        (
            relayed("daemon: Connection reset by peer (os error 104)"),
            CONTENTION_RETRY_DELAY,
        ),
        (
            relayed("daemon: index_warming: initial index is not published"),
            WARMING_RETRY_DELAY,
        ),
        (
            anyhow::Error::new(DaemonReplyTimeout::new(
                Duration::from_secs(1),
                Duration::from_secs(20),
            )),
            CONTENTION_RETRY_DELAY,
        ),
    ];
    for (error, delay) in &retry {
        assert_eq!(
            RetryPolicy::classify(RetryScope::Read, error, QUICK),
            RetryPolicy::Retry(*delay),
            "{error:#}"
        );
    }
    for error in [io(ErrorKind::WouldBlock), io(ErrorKind::TimedOut)] {
        assert_eq!(
            RetryPolicy::classify(RetryScope::Read, &error, FAST_TRANSPORT_FAILURE),
            RetryPolicy::Fail,
            "{error:#}"
        );
    }
    for error in [
        anyhow::anyhow!(coded!(TimedOut, "query deadline expired")),
        relayed("daemon: Resource temporarily unavailable (os error 11)"),
        relayed("daemon: timed out waiting for reply (os error 110)"),
        anyhow::anyhow!(coded!(StaleSource, "source revision changed")),
        anyhow::anyhow!(coded!(NoDefinition, "no indexed definition named `x`")),
        anyhow::anyhow!(coded!(InvalidCommand, "unknown command next")),
    ] {
        assert_eq!(
            RetryPolicy::classify(RetryScope::Read, &error, QUICK),
            RetryPolicy::Fail,
            "{error:#}"
        );
    }
}

#[test]
fn contention_only_replays_only_explicit_contention() {
    for error in [
        io(ErrorKind::BrokenPipe),
        io(ErrorKind::TimedOut),
        anyhow::Error::new(DaemonReplyTimeout::new(
            Duration::from_secs(1),
            Duration::from_secs(20),
        )),
    ] {
        assert_eq!(
            RetryPolicy::classify(RetryScope::ContentionOnly, &error, QUICK),
            RetryPolicy::Fail
        );
    }
    for error in [busy(), relayed("database is locked")] {
        assert_eq!(
            RetryPolicy::classify(RetryScope::ContentionOnly, &error, QUICK),
            RetryPolicy::Retry(CONTENTION_RETRY_DELAY)
        );
    }
}

#[test]
fn leading_timeout_answers_override_typed_io_retries() {
    let direct = terminal_io_timeout(ErrorKind::TimedOut, "writing_request");
    let daemon_wrapped = terminal_io_timeout(ErrorKind::TimedOut, "connecting")
        .with_coded_context(|| {
            CodedError::from_relayed("daemon: daemon: timed_out: phase=connecting".into())
                .expect("relayed timeout")
        })
        .context("retry loop received a relay error");

    for error in [&direct, &daemon_wrapped] {
        for scope in [
            RetryScope::Read,
            RetryScope::ContentionOnly,
            RetryScope::Never,
        ] {
            assert_eq!(
                RetryPolicy::classify(scope, error, QUICK),
                RetryPolicy::Fail,
                "scope={scope:?} error={error:#}"
            );
        }
    }

    let quoted_timeout = anyhow::Error::new(std::io::Error::new(
        ErrorKind::TimedOut,
        "daemon request write deadline expired",
    ))
    .with_coded_context(|| {
        coded!(
            InvalidBody,
            "submitted text says timed_out: phase=writing_request"
        )
    });
    assert_eq!(
        RetryPolicy::classify(RetryScope::Read, &quoted_timeout, QUICK),
        RetryPolicy::Retry(CONTENTION_RETRY_DELAY)
    );
}

#[test]
fn busy_replies_retry_by_leading_code_independent_of_explanation() {
    for message in [
        "daemon_busy: isolated injected queue refusal",
        "daemon: daemon_busy: worker pool is full",
        "daemon: daemon: daemon_busy: try another request later",
    ] {
        let error = relayed(message).context("route request through system router");
        for scope in [RetryScope::Read, RetryScope::ContentionOnly] {
            assert_eq!(
                RetryPolicy::classify(scope, &error, FAST_TRANSPORT_FAILURE),
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
fn busy_retry_hint_requires_a_leading_busy_code() {
    let hint = relayed(
        "daemon: daemon_busy: pool=root queue_depth=64 retry_after_ms=725 hint=retry_with_bounded_backoff",
    );
    assert_eq!(busy_retry_after(&hint), Some(Duration::from_millis(725)));
    for diagnostic in [
        "invalid_body: submitted text says daemon_busy: retry_after_ms=725",
        "daemon: invalid_options: daemon_busy: retry_after_ms=725",
    ] {
        assert_eq!(busy_retry_after(&relayed(diagnostic)), None);
    }
    let wrapped = anyhow::anyhow!(coded!(DaemonBusy, "retry_after_ms=725"))
        .with_coded_context(|| coded!(InvalidBody, "outer error"));
    assert_eq!(busy_retry_after(&wrapped), None);
    assert!(!is_explicit_busy(&wrapped));
    assert_eq!(
        RetryPolicy::classify(RetryScope::Read, &wrapped, QUICK),
        RetryPolicy::Fail
    );
}

#[test]
fn inner_timeout_warming_and_busy_codes_keep_their_retry_meaning() {
    let timeout = anyhow::anyhow!(coded!(TimedOut, "query deadline expired"))
        .with_coded_context(|| coded!(MemberUnavailable, "member answer"));
    assert_eq!(
        RetryPolicy::classify(RetryScope::Read, &timeout, QUICK),
        RetryPolicy::Fail
    );
    assert!(is_timeout_answer(&timeout));

    let warming = anyhow::anyhow!(coded!(IndexWarming, "initial index is not published"))
        .with_coded_context(|| coded!(MemberUnavailable, "member answer"));
    assert_eq!(
        RetryPolicy::classify(RetryScope::Read, &warming, QUICK),
        RetryPolicy::Retry(WARMING_RETRY_DELAY)
    );

    // A layer without a board classification does not hide the busy refusal.
    let busy = anyhow::anyhow!(coded!(DaemonBusy, "queue_depth=8 retry_after_ms=725"))
        .with_coded_context(|| coded!(SystemUnavailable, "router relay"));
    assert!(is_explicit_busy(&busy));
    assert_eq!(busy_retry_after(&busy), Some(Duration::from_millis(725)));
    assert_eq!(
        RetryPolicy::classify(RetryScope::ContentionOnly, &busy, QUICK),
        RetryPolicy::Retry(CONTENTION_RETRY_DELAY)
    );
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
            RetryPolicy::classify(RetryScope::ContentionOnly, &relayed(message), QUICK),
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
            RetryPolicy::classify(RetryScope::ContentionOnly, &relayed(message), QUICK),
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
        vec!["board", "inbox", "8"],
        vec!["board", "pending"],
        vec!["board", "receipt", &"a".repeat(64)],
        vec!["feedback", "ls"],
    ] {
        assert_eq!(RetryScope::for_options(&options(&words)), RetryScope::Read);
    }
    for words in [
        vec!["board"],
        vec!["board", "inbox"],
        vec!["board", "ingest"],
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
fn a_full_reply_wait_is_not_a_fast_transport_retry() {
    let error = io(ErrorKind::WouldBlock);
    assert_eq!(
        RetryPolicy::classify(
            RetryScope::Read,
            &error,
            tp_core::daemon_transport::PROXY_REPLY_WAIT
        ),
        RetryPolicy::Fail
    );
    assert_eq!(
        RetryPolicy::classify(
            RetryScope::Read,
            &busy(),
            tp_core::daemon_transport::PROXY_REPLY_WAIT
        ),
        RetryPolicy::Retry(CONTENTION_RETRY_DELAY)
    );
}
