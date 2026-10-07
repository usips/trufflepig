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
