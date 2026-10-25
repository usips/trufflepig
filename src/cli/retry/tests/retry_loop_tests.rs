use super::*;
use std::{
    cell::Cell,
    collections::VecDeque,
    fs,
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Instant,
};
use tp_core::coded_error::error_code;

struct FakeRun {
    result: Result<String>,
    attempts: usize,
    held_back: Vec<Duration>,
    waits: Vec<Duration>,
    fallbacks: usize,
    fallback_remaining: Vec<Duration>,
    elapsed: Duration,
}

fn run_fake(
    scope: RetryScope,
    request_id: &str,
    budget: Duration,
    results: Vec<(Result<String>, Duration)>,
    fallback: Option<Result<String>>,
) -> FakeRun {
    run_fake_with_wait(scope, request_id, budget, results, fallback, Duration::ZERO)
}

fn run_fake_with_wait(
    scope: RetryScope,
    request_id: &str,
    budget: Duration,
    results: Vec<(Result<String>, Duration)>,
    fallback: Option<Result<String>>,
    wait_overhead: Duration,
) -> FakeRun {
    let elapsed = Cell::new(Duration::ZERO);
    let attempts = Cell::new(0);
    let fallbacks = Cell::new(0);
    let mut held_back = Vec::new();
    let mut waits = Vec::new();
    let mut fallback_remaining = Vec::new();
    let mut results = VecDeque::from(results);
    let mut fallback = fallback;
    let result = run_retry_loop(
        scope,
        request_id,
        fallback.is_some(),
        || budget.saturating_sub(elapsed.get()),
        |delay| {
            waits.push(delay);
            elapsed.set(
                elapsed
                    .get()
                    .saturating_add(delay)
                    .saturating_add(wait_overhead),
            );
        },
        |invocation| match invocation {
            RetryInvocation::Attempt {
                index,
                held_back: reserve,
            } => {
                assert_eq!(index, attempts.get());
                attempts.set(index + 1);
                held_back.push(reserve);
                let (result, spent) = results.pop_front().expect("injected attempt result");
                elapsed.set(elapsed.get().saturating_add(spent));
                RetryOutput::Attempt(result, spent)
            }
            RetryInvocation::BusyFallback => {
                fallbacks.set(fallbacks.get() + 1);
                fallback_remaining.push(budget.saturating_sub(elapsed.get()));
                RetryOutput::BusyFallback(fallback.take())
            }
        },
    );
    FakeRun {
        result,
        attempts: attempts.get(),
        held_back,
        waits,
        fallbacks: fallbacks.get(),
        fallback_remaining,
        elapsed: elapsed.get(),
    }
}

#[test]
fn read_retries_recover_from_a_burst_with_deterministic_jitter() {
    let fake = run_fake(
        RetryScope::Read,
        "request-a",
        Duration::from_secs(20),
        vec![
            (Err(busy()), QUICK),
            (Err(busy()), QUICK),
            (Ok("recovered".into()), QUICK),
        ],
        None,
    );
    assert_eq!(fake.result.unwrap(), "recovered");
    assert_eq!(fake.attempts, 3);
    assert_eq!(fake.waits.len(), 2);
    assert_eq!(fake.fallbacks, 0);
    assert!(fake.waits[0] >= Duration::from_millis(250));
    assert!(fake.waits[0] <= Duration::from_millis(312));
    assert!(fake.waits[1] >= Duration::from_millis(375));
    assert!(fake.waits[1] <= Duration::from_millis(625));
    assert_ne!(backoff("request-a", 0), backoff("request-b", 0));
}

#[test]
fn retry_after_hint_is_honored_and_capped_at_two_seconds() {
    let fake = run_fake(
        RetryScope::Read,
        "hinted-read",
        Duration::from_secs(10),
        vec![
            (
                Err(anyhow::anyhow!(coded!(
                    DaemonBusy,
                    "pool=root retry_after_ms=5000"
                ))),
                QUICK,
            ),
            (Ok("recovered".into()), QUICK),
        ],
        None,
    );
    assert_eq!(fake.result.unwrap(), "recovered");
    assert_eq!(fake.attempts, 2);
    assert_eq!(fake.waits, vec![MAX_RETRY_BACKOFF]);
}

#[test]
fn one_absolute_read_budget_includes_attempt_time_and_backoff() {
    let fake = run_fake(
        RetryScope::Read,
        "budgeted-read",
        Duration::from_millis(2_200),
        vec![
            (Err(busy()), Duration::from_millis(500)),
            (Err(busy()), Duration::from_millis(500)),
        ],
        None,
    );
    assert!(fake.result.is_err());
    assert_eq!(fake.attempts, 2);
    assert_eq!(fake.waits.len(), 1);
    assert!(fake.elapsed > Duration::from_secs(1));
    assert!(fake.elapsed < Duration::from_millis(2_200));
    assert_eq!(fake.fallbacks, 1);
}

#[test]
fn a_full_initial_wait_leaves_no_read_retry_budget() {
    let fake = run_fake(
        RetryScope::Read,
        "full-wait",
        Duration::from_secs(20),
        vec![
            (Err(busy()), Duration::from_secs(20)),
            (Ok("late".into()), QUICK),
        ],
        None,
    );
    assert!(fake.result.is_err());
    assert_eq!(fake.attempts, 1);
    assert!(fake.waits.is_empty());
}

#[test]
fn waiting_past_the_deadline_never_starts_another_attempt() {
    let fake = run_fake_with_wait(
        RetryScope::Read,
        "overslept-read",
        Duration::from_secs(2),
        vec![(Err(busy()), QUICK), (Ok("late".into()), QUICK)],
        None,
        Duration::from_secs(2),
    );
    assert!(fake.result.is_err());
    assert_eq!(fake.attempts, 1);
    assert_eq!(fake.waits.len(), 1);
    assert_eq!(fake.fallbacks, 0);
}

#[test]
fn retry_reserve_skips_wait_when_delay_and_fallback_would_leave_no_useful_attempt() {
    let request_id = "near-boundary-read";
    let delay = retry_delay(request_id, 0, &busy());
    let reserve = retry_reserve(true);
    let budget = QUICK + delay + reserve;
    let fake = run_fake(
        RetryScope::Read,
        request_id,
        budget,
        vec![(Err(busy()), QUICK)],
        Some(Ok("local recovery".into())),
    );

    assert_eq!(fake.result.unwrap(), "local recovery");
    assert_eq!(fake.attempts, 1);
    assert!(fake.waits.is_empty());
    assert_eq!(fake.fallbacks, 1);
    assert_eq!(fake.fallback_remaining, vec![delay + reserve]);
}

#[test]
fn oversleep_keeps_available_time_for_the_original_busy_fallback() {
    let request_id = "overslept-busy-read";
    let delay = retry_delay(request_id, 0, &busy());
    let reserve = retry_reserve(true);
    let oversleep = Duration::from_millis(50);
    let budget = QUICK + delay + reserve + oversleep;
    let fake = run_fake_with_wait(
        RetryScope::Read,
        request_id,
        budget,
        vec![(Err(busy()), QUICK)],
        Some(Ok("local recovery".into())),
        oversleep,
    );

    assert_eq!(fake.result.unwrap(), "local recovery");
    assert_eq!(fake.attempts, 1);
    assert_eq!(fake.waits, vec![delay]);
    assert_eq!(fake.fallbacks, 1);
    assert_eq!(fake.fallback_remaining, vec![reserve]);
}

#[test]
fn unavailable_busy_fallback_returns_the_original_busy_error() {
    let request_id = "busy-without-fallback";
    let delay = retry_delay(request_id, 0, &busy());
    let reserve = retry_reserve(false);
    let fake = run_fake(
        RetryScope::Read,
        request_id,
        QUICK + delay + reserve,
        vec![(Err(busy()), QUICK)],
        None,
    );

    let error = fake.result.unwrap_err();
    assert_eq!(error_code(&error), Some(ErrorCode::DaemonBusy));
    assert_eq!(fake.attempts, 1);
    assert!(fake.waits.is_empty());
    assert_eq!(fake.fallbacks, 1);
}

#[test]
fn timeout_at_the_held_back_reserve_uses_the_busy_fallback() {
    let timeout = || {
        anyhow::Error::new(DaemonReplyTimeout::new(
            Duration::from_secs(1),
            Duration::from_secs(20),
        ))
    };
    let request_id = "busy-then-capped-timeout";
    let budget = Duration::from_secs(20);
    let delay = retry_delay(request_id, 0, &busy());
    // A capped attempt spends everything except the reserve before it times out.
    let capped_attempt = budget - QUICK - delay - BUSY_FALLBACK_RESERVE;
    let fake = run_fake(
        RetryScope::Read,
        request_id,
        budget,
        vec![(Err(busy()), QUICK), (Err(timeout()), capped_attempt)],
        Some(Ok("local recovery".into())),
    );

    assert_eq!(fake.result.unwrap(), "local recovery");
    assert_eq!(fake.attempts, 2);
    assert_eq!(fake.waits, vec![delay]);
    assert_eq!(
        fake.held_back,
        vec![BUSY_FALLBACK_RESERVE, BUSY_FALLBACK_RESERVE]
    );
    assert_eq!(fake.fallbacks, 1);
    assert!(fake.fallback_remaining[0] >= BUSY_FALLBACK_RESERVE);
}

#[test]
fn silent_first_attempt_at_the_reserve_uses_the_busy_fallback() {
    let timeout = || {
        anyhow::Error::new(DaemonReplyTimeout::new(
            Duration::from_secs(1),
            Duration::from_secs(20),
        ))
    };
    let budget = Duration::from_secs(30);
    // An accepted request that never answers spends everything except the reserve.
    let silent_attempt = budget - BUSY_FALLBACK_RESERVE;
    let fake = run_fake(
        RetryScope::Read,
        "silent-first-attempt",
        budget,
        vec![(Err(timeout()), silent_attempt)],
        Some(Ok("local recovery".into())),
    );

    assert_eq!(fake.result.unwrap(), "local recovery");
    assert_eq!(fake.attempts, 1);
    assert_eq!(fake.held_back, vec![BUSY_FALLBACK_RESERVE]);
    assert_eq!(fake.fallbacks, 1);
    assert!(fake.fallback_remaining[0] >= BUSY_FALLBACK_RESERVE);
}

#[test]
fn attempt_deadline_leaves_the_busy_fallback_reserve_only_when_held_back() {
    let read = QueryDeadline::after(Duration::from_secs(2));
    let full = attempt_deadline(&read, Duration::ZERO);
    assert!(full.remaining() > Duration::from_millis(1_900));
    let capped = attempt_deadline(&read, BUSY_FALLBACK_RESERVE);
    assert!(capped.remaining() <= Duration::from_millis(1_750));
    assert!(capped.remaining() > Duration::from_millis(1_500));
    assert!(read.remaining() > Duration::from_millis(1_900));
    let spent = attempt_deadline(&QueryDeadline::after(Duration::ZERO), BUSY_FALLBACK_RESERVE);
    assert!(spent.expired());
}

#[test]
fn early_timeout_after_a_busy_retry_keeps_the_timeout_error() {
    let timeout = || {
        anyhow::Error::new(DaemonReplyTimeout::new(
            Duration::from_secs(1),
            Duration::from_secs(20),
        ))
    };
    let fake = run_fake(
        RetryScope::Read,
        "busy-then-timeout",
        Duration::from_secs(20),
        vec![(Err(busy()), QUICK)]
            .into_iter()
            .chain((0..READ_ATTEMPTS - 1).map(|_| (Err(timeout()), QUICK)))
            .collect(),
        Some(Ok("must not run".into())),
    );

    let error = fake.result.unwrap_err();
    assert!(error.downcast_ref::<DaemonReplyTimeout>().is_some());
    assert_eq!(fake.attempts, READ_ATTEMPTS);
    assert_eq!(fake.waits.len(), READ_ATTEMPTS - 1);
    assert_eq!(fake.fallbacks, 0);
}

#[test]
fn busy_seen_keeps_fallback_reserve_after_a_quick_typed_timeout() {
    let request_id = "busy-then-quick-timeout-boundary";
    let timeout = || {
        anyhow::Error::new(DaemonReplyTimeout::new(
            Duration::from_millis(1),
            Duration::from_secs(20),
        ))
    };
    let first_delay = retry_delay(request_id, 0, &busy());
    let next_delay = retry_delay(request_id, 1, &timeout());
    let budget = QUICK
        + first_delay
        + QUICK
        + next_delay
        + USEFUL_ATTEMPT_FLOOR
        + Duration::from_millis(100);
    let fake = run_fake(
        RetryScope::Read,
        request_id,
        budget,
        vec![(Err(busy()), QUICK), (Err(timeout()), QUICK)],
        Some(Ok("must not run".into())),
    );

    let error = fake.result.unwrap_err();
    assert!(error.downcast_ref::<DaemonReplyTimeout>().is_some());
    assert_eq!(fake.attempts, 2);
    assert_eq!(fake.waits, vec![first_delay]);
    assert_eq!(
        fake.held_back,
        vec![BUSY_FALLBACK_RESERVE, BUSY_FALLBACK_RESERVE]
    );
    assert_eq!(fake.fallbacks, 0);
}

#[test]
fn unbusy_quick_timeouts_can_use_the_same_boundary_budget() {
    let request_id = "timeouts-without-busy-boundary";
    let timeout = || {
        anyhow::Error::new(DaemonReplyTimeout::new(
            Duration::from_millis(1),
            Duration::from_secs(20),
        ))
    };
    let first_delay = retry_delay(request_id, 0, &timeout());
    let next_delay = retry_delay(request_id, 1, &timeout());
    let budget = QUICK
        + first_delay
        + QUICK
        + next_delay
        + USEFUL_ATTEMPT_FLOOR
        + Duration::from_millis(100);
    let fake = run_fake(
        RetryScope::Read,
        request_id,
        budget,
        vec![
            (Err(timeout()), QUICK),
            (Err(timeout()), QUICK),
            (Ok("recovered".into()), QUICK),
        ],
        None,
    );

    assert_eq!(fake.result.unwrap(), "recovered");
    assert_eq!(fake.attempts, 3);
    assert_eq!(fake.waits, vec![first_delay, next_delay]);
    assert_eq!(fake.held_back, vec![Duration::ZERO; 3]);
    assert_eq!(fake.fallbacks, 0);
}

#[test]
fn expired_busy_fallback_keeps_the_busy_reply_code() {
    let fake = run_fake(
        RetryScope::Read,
        "expired-busy-fallback",
        QUICK + Duration::from_millis(5),
        vec![(
            Err(relayed("daemon: daemon_busy: pool=root retry_after_ms=250")),
            QUICK,
        )],
        Some(Err(anyhow::anyhow!(coded!(
            TimedOut,
            "query deadline expired"
        )))),
    );

    let error = fake.result.unwrap_err();
    assert_eq!(error_code(&error), Some(ErrorCode::DaemonBusy));
    assert!(format!("{error:#}").contains("timed_out: query deadline expired"));
    assert!(format!("{error:#}").contains("retry_after_ms=250"));
    assert_eq!(fake.attempts, 1);
    assert_eq!(fake.fallbacks, 1);
}

#[test]
fn short_cli_budget_uses_one_busy_router_attempt_then_current_path_bytes() {
    assert_busy_fallback_child(
        "short_deadline_cli_busy_fallback_child",
        "SHORT_DEADLINE_FALLBACK_ACCEPTED",
    );
}

#[test]
fn held_reserve_busy_retry_leaves_the_fallback_reserve() {
    assert_busy_fallback_child(
        "held_reserve_cli_busy_fallback_child",
        "HELD_RESERVE_FALLBACK_ACCEPTED",
    );
}

fn assert_busy_fallback_child(child: &str, marker: &str) {
    let (_temp, mut command) = busy_fallback_child_command(child);
    let output = run_retry_test_child(&mut command, Duration::from_secs(6));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "isolated retry test child failed\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains(marker),
        "isolated child did not report the accepted router and local fallback\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// Builds a child process that runs `child` against isolated paths and a fake router socket.
fn busy_fallback_child_command(child: &str) -> (tempfile::TempDir, Command) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let system_dir = temp.path().join("system");
    let spool_dir = temp.path().join("spool");
    let cache = temp.path().join("blocked-cache");
    let home = temp.path().join("home");
    let config = temp.path().join("config");
    let xdg_cache = temp.path().join("xdg-cache");
    let data = temp.path().join("data");
    let runtime = temp.path().join("runtime");
    let scratch = temp.path().join("scratch");
    for directory in [
        root.join("docs"),
        system_dir.clone(),
        spool_dir.clone(),
        home.clone(),
        config.clone(),
        xdg_cache.clone(),
        data.clone(),
        runtime.clone(),
        scratch.clone(),
    ] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::write(root.join("docs/guide.md"), "# local current path\n").unwrap();
    fs::write(&cache, "preserve this cache file").unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_CACHE_HOME", &xdg_cache)
        .env("XDG_DATA_HOME", &data)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("TRUFFLEPIG_SYSTEM_DIR", &system_dir)
        .env("TRUFFLEPIG_SPOOL_DIR", &spool_dir)
        .env("TRUFFLEPIG_RETRY_TEST_CHILD", "1")
        .env("TRUFFLEPIG_RETRY_TEST_ROOT", &root)
        .env("TRUFFLEPIG_RETRY_TEST_CACHE", &cache)
        .env("TMPDIR", &scratch)
        .arg("--ignored")
        .arg(child)
        .arg("--nocapture");
    (temp, command)
}

#[test]
#[ignore = "invoked by short_cli_budget_uses_one_busy_router_attempt_then_current_path_bytes"]
fn short_deadline_cli_busy_fallback_child() {
    if std::env::var_os("TRUFFLEPIG_RETRY_TEST_CHILD").is_none() {
        return;
    }
    let requests = current_path_fallback_under_held_router(Duration::from_millis(1_500));
    assert_eq!(
        requests.len(),
        1,
        "a second transport attempt consumes the fallback window"
    );
    println!(
        "SHORT_DEADLINE_FALLBACK_ACCEPTED requests={}",
        requests.len()
    );
}

#[test]
#[ignore = "invoked by held_reserve_busy_retry_leaves_the_fallback_reserve"]
fn held_reserve_cli_busy_fallback_child() {
    if std::env::var_os("TRUFFLEPIG_RETRY_TEST_CHILD").is_none() {
        return;
    }
    let requests = current_path_fallback_under_held_router(Duration::from_secs(4));
    assert_eq!(
        requests.len(),
        2,
        "the second attempt must run with the busy reserve held back"
    );
    println!("HELD_RESERVE_FALLBACK_ACCEPTED requests={}", requests.len());
}

/// Answers the first request busy and holds later sockets open, so only the
/// client's own deadline or the local fallback can finish the command.
fn current_path_fallback_under_held_router(budget: Duration) -> Vec<Vec<String>> {
    const REQUEST_LIMIT: usize = 64 * 1024;
    const BUSY_REPLY: &[u8] = br#"{"status":"failure","message":"daemon_busy: pool=router-query queue_depth=64 retry_after_ms=250"}"#;

    let root = std::path::PathBuf::from(std::env::var_os("TRUFFLEPIG_RETRY_TEST_ROOT").unwrap());
    let cache = std::path::PathBuf::from(std::env::var_os("TRUFFLEPIG_RETRY_TEST_CACHE").unwrap());
    let system_dir = std::path::PathBuf::from(std::env::var_os("TRUFFLEPIG_SYSTEM_DIR").unwrap());
    let listener =
        UnixListener::bind(system_dir.join(tp_core::daemon_transport::SOCKET_NAME)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let requests = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let stopping = Arc::new(AtomicBool::new(false));
    let thread_requests = Arc::clone(&requests);
    let thread_stopping = Arc::clone(&stopping);
    let router = thread::spawn(move || {
        let expiry = Instant::now() + Duration::from_secs(5);
        while !thread_stopping.load(Ordering::Acquire) && Instant::now() < expiry {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .expect("bound retry test request reads");
                    let request = read_retry_request(&mut stream, REQUEST_LIMIT)
                        .expect("read retry test request");
                    let attempt = {
                        let mut requests = thread_requests.lock().unwrap();
                        requests.push(request);
                        requests.len()
                    };
                    if attempt == 1 {
                        write_retry_frame(&mut stream, BUSY_REPLY).expect("reply with busy");
                    } else {
                        while !thread_stopping.load(Ordering::Acquire) && Instant::now() < expiry {
                            thread::sleep(Duration::from_millis(2));
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("retry test router accept failed: {error}"),
            }
        }
    });

    let args = vec![
        "--root".to_owned(),
        root.display().to_string(),
        "--cache".to_owned(),
        cache.display().to_string(),
        "--no-workspace".to_owned(),
        "--diagnostics".to_owned(),
        "off".to_owned(),
        "--budget".to_owned(),
        "1000".to_owned(),
        "show".to_owned(),
        "path:docs/guide.md".to_owned(),
    ];
    let options = super::super::super::parse(&args).expect("parse current-path CLI args");
    let context = RequestContext::new(Some("short-deadline-child".into()), Some("codex".into()));
    let result = run_with_retry_deadline(
        &args,
        &options,
        &context,
        Some(QueryDeadline::after(budget)),
    );
    stopping.store(true, Ordering::Release);
    router.join().expect("join retry test router");

    let response: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
    assert_eq!(response["source"], "current_file");
    assert_eq!(response["verified"], false);
    assert_eq!(response["lines"][0]["text"], "# local current path\n");
    assert!(cache.is_file());
    assert_eq!(
        fs::read_to_string(cache).unwrap(),
        "preserve this cache file"
    );
    let requests = requests.lock().unwrap().clone();
    assert!(
        requests[0]
            .iter()
            .any(|argument| argument == "path:docs/guide.md"),
        "router did not receive the current-path show request: {requests:?}"
    );
    requests
}

fn read_retry_request(
    stream: &mut UnixStream,
    request_limit: usize,
) -> std::io::Result<Vec<String>> {
    let mut header = [0_u8; 4];
    stream.read_exact(&mut header)?;
    let length = u32::from_be_bytes(header) as usize;
    if length > request_limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "retry test request exceeds frame limit",
        ));
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    Ok(value["arguments"]["args"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_owned)
        .collect())
}

fn write_retry_frame(stream: &mut UnixStream, bytes: &[u8]) -> std::io::Result<()> {
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(bytes)?;
    stream.flush()
}

fn run_retry_test_child(command: &mut Command, timeout: Duration) -> std::process::Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn isolated retry test child");
    let expiry = Instant::now() + timeout;
    loop {
        if child
            .try_wait()
            .expect("check isolated retry test child")
            .is_some()
        {
            break;
        }
        if Instant::now() >= expiry {
            let _ = child.kill();
            let output = child
                .wait_with_output()
                .expect("collect timed-out retry test child output");
            panic!(
                "isolated retry test child exceeded {timeout:?}\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
    child
        .wait_with_output()
        .expect("collect isolated retry test child output")
}

#[test]
fn read_attempts_stop_at_five_then_use_busy_only_fallback() {
    let fake = run_fake(
        RetryScope::Read,
        "read-exhaustion",
        Duration::from_secs(20),
        (0..READ_ATTEMPTS).map(|_| (Err(busy()), QUICK)).collect(),
        Some(Ok("local recovery".into())),
    );
    assert_eq!(fake.result.unwrap(), "local recovery");
    assert_eq!(fake.attempts, 5);
    assert_eq!(fake.waits.len(), 4);
    assert_eq!(fake.fallbacks, 1);
}

#[test]
fn diagnostic_busy_lock_and_typed_timeout_do_not_use_busy_fallback() {
    for (kind, expected_attempts) in [
        ("diagnostic", 1),
        ("wrapped_busy", 1),
        ("database_locked", 5),
        ("timeout", 5),
    ] {
        let make_error = || match kind {
            "diagnostic" => anyhow::anyhow!(coded!(InvalidBody, "daemon_busy: retry_after_ms=725")),
            "wrapped_busy" => anyhow::anyhow!(coded!(DaemonBusy, "retry_after_ms=725"))
                .with_coded_context(|| coded!(InvalidBody, "outer error")),
            "database_locked" => relayed("database is locked"),
            "timeout" => anyhow::Error::new(DaemonReplyTimeout::new(
                Duration::from_secs(1),
                Duration::from_secs(20),
            )),
            _ => unreachable!(),
        };
        let fake = run_fake(
            RetryScope::Read,
            "non-busy-read",
            Duration::from_secs(20),
            (0..READ_ATTEMPTS)
                .map(|_| (Err(make_error()), QUICK))
                .collect(),
            Some(Ok("must not run".into())),
        );
        assert!(fake.result.is_err());
        assert_eq!(fake.attempts, expected_attempts);
        assert_eq!(fake.fallbacks, 0);
    }
}

#[test]
fn terminal_write_timeouts_stop_before_retry_or_local_fallback() {
    for (kind, phase) in [
        (ErrorKind::TimedOut, "writing_request"),
        (ErrorKind::TimedOut, "connecting"),
        (ErrorKind::WouldBlock, "writing_request"),
        (ErrorKind::WouldBlock, "connecting"),
    ] {
        let fake = run_fake(
            RetryScope::Read,
            "terminal-write-timeout",
            Duration::from_secs(20),
            (0..READ_ATTEMPTS)
                .map(|_| (Err(terminal_io_timeout(kind, phase)), QUICK))
                .collect(),
            Some(Ok("must not run".into())),
        );
        assert!(fake.result.is_err());
        assert_eq!(fake.attempts, 1);
        assert!(fake.waits.is_empty());
        assert_eq!(fake.fallbacks, 0);
    }
}

#[test]
fn contention_only_has_two_total_safe_attempts() {
    let fake = run_fake(
        RetryScope::ContentionOnly,
        "write-request",
        Duration::from_secs(20),
        vec![(Err(busy()), QUICK), (Err(busy()), QUICK)],
        Some(Ok("must not run".into())),
    );
    assert!(fake.result.is_err());
    assert_eq!(fake.attempts, CONTENTION_ATTEMPTS);
    assert_eq!(fake.waits.len(), 1);
    assert_eq!(fake.fallbacks, 0);
}

#[test]
fn writes_never_replay_transport_or_typed_reply_timeouts() {
    for failure in [
        io(ErrorKind::BrokenPipe),
        io(ErrorKind::TimedOut),
        anyhow::Error::new(DaemonReplyTimeout::new(
            Duration::from_secs(1),
            Duration::from_secs(20),
        )),
    ] {
        let fake = run_fake(
            RetryScope::ContentionOnly,
            "write-request",
            Duration::from_secs(20),
            vec![(Err(failure), QUICK), (Ok("must not replay".into()), QUICK)],
            Some(Ok("must not run".into())),
        );
        assert!(fake.result.is_err());
        assert_eq!(fake.attempts, 1);
        assert!(fake.waits.is_empty());
        assert_eq!(fake.fallbacks, 0);
    }
}

#[test]
fn board_ingest_is_a_write_and_does_not_replay_typed_timeout() {
    let args = vec!["board".to_owned(), "ingest".to_owned()];
    let options = super::super::super::parse(&args).unwrap();
    let scope = RetryScope::for_options(&options);
    assert_eq!(scope, RetryScope::ContentionOnly);
    let fake = run_fake(
        scope,
        "board-ingest",
        Duration::from_secs(20),
        vec![
            (
                Err(anyhow::Error::new(DaemonReplyTimeout::new(
                    Duration::from_secs(1),
                    Duration::from_secs(20),
                ))),
                QUICK,
            ),
            (Ok("must not replay".into()), QUICK),
        ],
        None,
    );
    assert!(fake.result.is_err());
    assert_eq!(fake.attempts, 1);
    assert!(fake.waits.is_empty());
}
