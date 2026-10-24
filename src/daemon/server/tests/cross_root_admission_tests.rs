use super::*;
use crate::{
    daemon::{RequestPool, pool::PooledJob},
    daemon_transport::request_by,
    query_deadline::QueryDeadline,
};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const CLIENT_BUDGET: Duration = Duration::from_secs(12);
const TEST_WAIT: Duration = Duration::from_secs(5);
const PROBE_RETRY_BUDGET: Duration = Duration::from_secs(3);
const PROBE_RETRY_BACKOFF: Duration = Duration::from_millis(20);

#[derive(Debug)]
enum DisconnectEvent {
    Forwarded(String),
    Returned(String, String),
    ClientReply(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockedLane {
    RegexA,
    MaintenanceA,
}

#[derive(Default)]
struct LaneCounts {
    regex_calls: AtomicUsize,
    regex_active: AtomicUsize,
    regex_max: AtomicUsize,
    maintenance_calls: AtomicUsize,
    maintenance_active: AtomicUsize,
    maintenance_max: AtomicUsize,
}

struct ActiveLane<'a>(&'a AtomicUsize);

impl Drop for ActiveLane<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn enter_lane<'a>(active: &'a AtomicUsize, maximum: &AtomicUsize) -> ActiveLane<'a> {
    let current = active.fetch_add(1, Ordering::AcqRel) + 1;
    maximum.fetch_max(current, Ordering::AcqRel);
    ActiveLane(active)
}

type ReleaseGate = Arc<(Mutex<bool>, Condvar)>;

struct ReleaseGates {
    regex_a: ReleaseGate,
    maintenance_a: ReleaseGate,
    released: bool,
}

impl ReleaseGates {
    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        for gate in [&self.regex_a, &self.maintenance_a] {
            let (released, changed) = &**gate;
            if let Ok(mut released) = released.lock() {
                *released = true;
                changed.notify_all();
            }
        }
    }
}

impl Drop for ReleaseGates {
    fn drop(&mut self) {
        self.release();
    }
}

fn wait_gate(gate: &ReleaseGate) {
    let (released, changed) = &**gate;
    let Ok(released) = released.lock() else {
        return;
    };
    let _ = changed.wait_timeout_while(released, Duration::from_secs(15), |released| !*released);
}

fn scripted_root(
    label: &'static str,
    counts: Arc<LaneCounts>,
    block_a: bool,
    regex_release: ReleaseGate,
    maintenance_release: ReleaseGate,
    blocked: mpsc::Sender<BlockedLane>,
) -> TestDaemon {
    let regex_holds = AtomicUsize::new(0);
    let maintenance_holds = AtomicUsize::new(0);
    TestDaemon {
        reply: Box::new(move |request| {
            let root_index = request
                .args
                .iter()
                .position(|argument| argument == "--root")
                .expect("test requests include --root");
            let verb = request.args[root_index + 2].as_str();
            let subcommand = request.args.get(root_index + 3).map_or("", String::as_str);
            let regex = verb == "search"
                && request.args[root_index + 3..]
                    .iter()
                    .any(|argument| argument.starts_with("re:"));
            let maintenance = verb == "semantic" && subcommand == "prepare";

            if regex {
                counts.regex_calls.fetch_add(1, Ordering::AcqRel);
                let _active = enter_lane(&counts.regex_active, &counts.regex_max);
                let hold = request.args[root_index + 3..]
                    .iter()
                    .any(|argument| argument == "re:hold");
                if block_a && hold && regex_holds.fetch_add(1, Ordering::AcqRel) < 2 {
                    let _ = blocked.send(BlockedLane::RegexA);
                    wait_gate(&regex_release);
                }
                return Ok(format!("{label}:{}", request.args.join("|")));
            }

            if maintenance {
                counts.maintenance_calls.fetch_add(1, Ordering::AcqRel);
                let _active = enter_lane(&counts.maintenance_active, &counts.maintenance_max);
                if block_a && maintenance_holds.fetch_add(1, Ordering::AcqRel) == 0 {
                    let _ = blocked.send(BlockedLane::MaintenanceA);
                    wait_gate(&maintenance_release);
                }
            }
            Ok(format!("{label}:{}", request.args.join("|")))
        }),
        reconcile: quiet(),
    }
}

struct RunningDaemons {
    router: Option<Served>,
    root_a: Option<Served>,
    root_b: Option<Served>,
}

impl Drop for RunningDaemons {
    fn drop(&mut self) {
        for daemon in [&mut self.router, &mut self.root_a, &mut self.root_b] {
            if let Some(daemon) = daemon.take() {
                let _ = daemon.stop();
            }
        }
    }
}

struct PendingRequest {
    label: &'static str,
    receiver: mpsc::Receiver<anyhow::Result<Option<String>>>,
    thread: JoinHandle<()>,
}

impl PendingRequest {
    fn wait(self) -> anyhow::Result<Option<String>> {
        let result = self
            .receiver
            .recv_timeout(TEST_WAIT)
            .unwrap_or_else(|error| panic!("{} did not finish: {error}", self.label));
        self.thread.join().expect("request thread panicked");
        result
    }
}

fn request_async(cache: PathBuf, args: Vec<String>, label: &'static str) -> PendingRequest {
    let (sender, receiver) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let result = request_by(
            &cache,
            &args,
            &RequestContext::new(None, None),
            QueryDeadline::after(CLIENT_BUDGET),
        );
        let _ = sender.send(result);
    });
    PendingRequest {
        label,
        receiver,
        thread,
    }
}

fn direct_args(root: &Path, words: &[&str]) -> Vec<String> {
    let mut args = vec![
        "--no-workspace".to_owned(),
        "--root".to_owned(),
        root.display().to_string(),
    ];
    args.extend(words.iter().map(|word| (*word).to_owned()));
    args
}

fn workspace_args(config: &Path, root: &Path, words: &[&str]) -> Vec<String> {
    let mut args = vec![
        "--workspace".to_owned(),
        config.display().to_string(),
        "--root".to_owned(),
        root.display().to_string(),
    ];
    args.extend(words.iter().map(|word| (*word).to_owned()));
    args
}

fn write_workspace(directory: &Path, name: &str, home: &Path, shared: &Path) -> PathBuf {
    let config = directory.join(format!("{name}.toml"));
    std::fs::write(
        &config,
        format!(
            "[workspace]\nname='{name}'\n[members.home]\npath='{}'\n[members.shared]\npath='{}'\n",
            home.display(),
            shared.display(),
        ),
    )
    .unwrap();
    config
}

fn wait_for_blocked(receiver: &mpsc::Receiver<BlockedLane>, expected: BlockedLane) {
    assert_eq!(
        receiver
            .recv_timeout(TEST_WAIT)
            .expect("root handler did not enter"),
        expected
    );
}

fn assert_root_limit(request: PendingRequest, pool: &str, outstanding: usize, capacity: usize) {
    let error = request
        .wait()
        .expect_err("root outstanding limit should refuse this request");
    let message = format!("{error:#}");
    assert!(message.contains(&format!("pool={pool}")), "{message}");
    assert!(message.contains("cause=root_limit"), "{message}");
    assert!(
        message.contains(&format!(
            "root_outstanding={outstanding} root_capacity={capacity}"
        )),
        "{message}"
    );
}

fn forwarded_replacement(
    cache: PathBuf,
    args: Vec<String>,
    forwarded: &mpsc::Receiver<(String, String, String)>,
) -> PendingRequest {
    let started = Instant::now();
    loop {
        assert!(
            started.elapsed() < PROBE_RETRY_BUDGET,
            "disconnected request kept the root reservation through the retry budget"
        );
        let request = request_async(cache.clone(), args.clone(), "root A replacement probe");
        loop {
            match request.receiver.try_recv() {
                Ok(result) => {
                    request
                        .thread
                        .join()
                        .expect("probe request thread panicked");
                    let error = result.expect_err("replacement probe unexpectedly completed");
                    let message = format!("{error:#}");
                    assert!(
                        message.contains("cause=root_limit"),
                        "unexpected replacement probe refusal: {message}"
                    );
                    break;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("replacement probe request thread ended without a result")
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }

            match forwarded.try_recv() {
                Ok(event) => {
                    assert_eq!(
                        (event.0.as_str(), event.1.as_str(), event.2.as_str()),
                        ("a", "search", "re:replacement")
                    );
                    return request;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("replacement forwarding event channel closed")
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }

            let remaining = PROBE_RETRY_BUDGET.saturating_sub(started.elapsed());
            assert!(
                !remaining.is_zero(),
                "replacement probe was neither refused nor forwarded within the retry budget"
            );
            std::thread::sleep(PROBE_RETRY_BACKOFF.min(remaining));
        }

        let remaining = PROBE_RETRY_BUDGET.saturating_sub(started.elapsed());
        assert!(
            !remaining.is_zero(),
            "disconnected request kept the root reservation through the retry budget"
        );
        std::thread::sleep(PROBE_RETRY_BACKOFF.min(remaining));
    }
}

fn disconnect_forwarded_request(
    cache: &Path,
    config: &Path,
    root: &Path,
    events_tx: &mpsc::Sender<DisconnectEvent>,
    events_rx: &mpsc::Receiver<DisconnectEvent>,
) {
    let query = "re:disconnect".to_owned();
    let args = workspace_args(config, root, &["search", "ws:all", &query]);
    let mut client = UnixStream::connect(cache.join(crate::daemon_transport::SOCKET_NAME)).unwrap();
    crate::daemon_transport::daemon_wire_protocol::write_request(
        &mut client,
        &crate::daemon_transport::daemon_wire_protocol::DaemonRequest::Arguments {
            context: crate::request_context::RequestContext::new(None, None),
            args,
        },
    )
    .unwrap();

    let mut reader = client.try_clone().unwrap();
    reader.set_read_timeout(Some(TEST_WAIT)).unwrap();
    let (events_tx, query_for_reader) = (events_tx.clone(), query.clone());
    let reply_reader = std::thread::spawn(move || {
        let _ = crate::daemon_transport::daemon_wire_protocol::read_reply(&mut reader);
        let _ = events_tx.send(DisconnectEvent::ClientReply(query_for_reader));
    });

    let admitted = events_rx
        .recv_timeout(TEST_WAIT)
        .unwrap_or_else(|error| panic!("disconnect request was not admitted: {error}"));
    match admitted {
        DisconnectEvent::Forwarded(forwarded_query) if forwarded_query == query => {}
        event => {
            let _ = client.shutdown(std::net::Shutdown::Both);
            drop(client);
            let _ = reply_reader.join();
            panic!("disconnect request did not reach the router handler: {event:?}");
        }
    }

    client.shutdown(std::net::Shutdown::Both).unwrap();
    drop(client);
    let mut returned = None;
    let mut client_reply_seen = false;
    while returned.is_none() || !client_reply_seen {
        match events_rx
            .recv_timeout(TEST_WAIT)
            .unwrap_or_else(|error| panic!("forwarded disconnect did not return: {error}"))
        {
            DisconnectEvent::Returned(returned_query, detail) if returned_query == query => {
                assert!(returned.replace(detail).is_none(), "duplicate return event");
            }
            DisconnectEvent::ClientReply(reply_query) if reply_query == query => {
                assert!(!client_reply_seen, "duplicate client reply event");
                client_reply_seen = true;
            }
            event => panic!("unexpected disconnect completion event: {event:?}"),
        }
    }
    reply_reader.join().expect("reply reader panicked");
    let returned = returned.expect("forwarding completion event was observed");
    assert!(
        returned.starts_with("daemon_reply_timeout:")
            || returned.starts_with("timed_out: phase=connecting")
            || returned.starts_with("timed_out: phase=writing_request"),
        "disconnect returned an unexpected forwarding result: {returned}"
    );
}

#[test]
fn router_caps_overlapping_workspace_work_while_roots_keep_other_lanes_live() {
    let (blocked_tx, blocked_rx) = mpsc::channel();
    let regex_release = Arc::new((Mutex::new(false), Condvar::new()));
    let maintenance_release = Arc::new((Mutex::new(false), Condvar::new()));
    let root_a_counts = Arc::new(LaneCounts::default());
    let root_b_counts = Arc::new(LaneCounts::default());
    let root_a = Served::start_tier(
        AdmissionTier::Root,
        false,
        WIDE,
        scripted_root(
            "root-a",
            Arc::clone(&root_a_counts),
            true,
            Arc::clone(&regex_release),
            Arc::clone(&maintenance_release),
            blocked_tx.clone(),
        ),
    );
    let root_b = Served::start_tier(
        AdmissionTier::Root,
        false,
        WIDE,
        scripted_root(
            "root-b",
            Arc::clone(&root_b_counts),
            false,
            Arc::clone(&regex_release),
            Arc::clone(&maintenance_release),
            blocked_tx.clone(),
        ),
    );
    let root_a_path = root_a.root.clone();
    let root_b_path = root_b.root.clone();
    let cache_a = root_a.cache.clone();
    let cache_b = root_b.cache.clone();
    let routed_root_a_path = root_a_path.clone();
    let routed_root_b_path = root_b_path.clone();
    let (forwarded_tx, forwarded_rx) = mpsc::channel::<(String, String, String)>();
    let (disconnect_events_tx, disconnect_events_rx) = mpsc::channel::<DisconnectEvent>();
    let routed_disconnect_events_tx = disconnect_events_tx.clone();
    let router = Served::start_tier(
        AdmissionTier::Router,
        false,
        WIDE,
        TestDaemon {
            reply: Box::new(move |request| {
                let root_index = request
                    .args
                    .iter()
                    .position(|argument| argument == "--root")
                    .expect("test requests include --root");
                let root = &request.args[root_index + 1];
                let (label, cache) =
                    if root.as_str() == routed_root_a_path.to_string_lossy().as_ref() {
                        ("a", &cache_a)
                    } else if root.as_str() == routed_root_b_path.to_string_lossy().as_ref() {
                        ("b", &cache_b)
                    } else {
                        anyhow::bail!("unknown test root")
                    };
                let verb = request.args[root_index + 2].clone();
                let query = request.args[root_index + 3..]
                    .iter()
                    .find(|argument| argument.starts_with("re:"))
                    .cloned()
                    .unwrap_or_default();
                let disconnected = query == "re:disconnect";
                if disconnected {
                    let _ =
                        routed_disconnect_events_tx.send(DisconnectEvent::Forwarded(query.clone()));
                } else {
                    let _ = forwarded_tx.send((label.to_owned(), verb, query.clone()));
                }
                let result = request_by(
                    cache,
                    &request.args,
                    &request.context,
                    request.deadline.clone(),
                );
                if disconnected {
                    let detail = result
                        .as_ref()
                        .err()
                        .map(|error| format!("{error:#}"))
                        .unwrap_or_else(|| "unexpected success".to_owned());
                    let _ =
                        routed_disconnect_events_tx.send(DisconnectEvent::Returned(query, detail));
                }
                result?.ok_or_else(|| anyhow::anyhow!("scripted root stopped before forwarding"))
            }),
            reconcile: quiet(),
        },
    );
    let router_cache = router.cache.clone();
    let mut daemons = RunningDaemons {
        router: Some(router),
        root_a: Some(root_a),
        root_b: Some(root_b),
    };
    let mut release = ReleaseGates {
        regex_a: regex_release,
        maintenance_a: maintenance_release,
        released: false,
    };

    let workspace_files = tempfile::tempdir().unwrap();
    let shared_member = tempfile::tempdir().unwrap();
    let config_a = write_workspace(
        workspace_files.path(),
        "workspace-a",
        &root_a_path,
        shared_member.path(),
    );
    let config_b = write_workspace(
        workspace_files.path(),
        "workspace-b",
        &root_b_path,
        shared_member.path(),
    );

    let mut regex_active = Vec::with_capacity(2);
    for _ in 0..2 {
        regex_active.push(request_async(
            router_cache.clone(),
            workspace_args(&config_a, &root_a_path, &["search", "ws:all", "re:hold"]),
            "root A regex request",
        ));
    }
    wait_for_blocked(&blocked_rx, BlockedLane::RegexA);
    wait_for_blocked(&blocked_rx, BlockedLane::RegexA);
    for _ in 0..2 {
        let (root, verb, query) = forwarded_rx.recv_timeout(TEST_WAIT).unwrap();
        assert_eq!(
            (root.as_str(), verb.as_str(), query.as_str()),
            ("a", "search", "re:hold")
        );
    }

    disconnect_forwarded_request(
        &router_cache,
        &config_a,
        &root_a_path,
        &disconnect_events_tx,
        &disconnect_events_rx,
    );

    let regex_replacement = forwarded_replacement(
        router_cache.clone(),
        workspace_args(
            &config_a,
            &root_a_path,
            &["search", "ws:all", "re:replacement"],
        ),
        &forwarded_rx,
    );
    assert_root_limit(
        request_async(
            router_cache.clone(),
            workspace_args(
                &config_a,
                &root_a_path,
                &["search", "ws:all", "re:overflow"],
            ),
            "root A excess regex request",
        ),
        "test-regex",
        3,
        3,
    );
    assert_root_limit(
        request_async(
            router_cache.clone(),
            workspace_args(
                &config_b,
                &root_b_path,
                &["search", "in:shared", "re:overlap"],
            ),
            "overlapping workspace regex request",
        ),
        "test-regex",
        3,
        3,
    );

    let root_b_regex = request_async(
        router_cache.clone(),
        direct_args(&root_b_path, &["search", "re:independent"]),
        "disjoint root B regex request",
    );
    assert!(
        root_b_regex.wait().unwrap().unwrap().starts_with("root-b:"),
        "root B regex work waited behind root A"
    );
    let (root, verb, query) = forwarded_rx.recv_timeout(TEST_WAIT).unwrap();
    assert_eq!(
        (root.as_str(), verb.as_str(), query.as_str()),
        ("b", "search", "re:independent")
    );

    let maintenance_active = request_async(
        router_cache.clone(),
        workspace_args(&config_a, &root_a_path, &["semantic", "prepare"]),
        "root A semantic preparation",
    );
    wait_for_blocked(&blocked_rx, BlockedLane::MaintenanceA);
    let (root, verb, _) = forwarded_rx.recv_timeout(TEST_WAIT).unwrap();
    assert_eq!((root.as_str(), verb.as_str()), ("a", "semantic"));
    let maintenance_queued = request_async(
        router_cache.clone(),
        workspace_args(&config_a, &root_a_path, &["semantic", "prepare"]),
        "root A queued semantic preparation",
    );
    let (root, verb, _) = forwarded_rx.recv_timeout(TEST_WAIT).unwrap();
    assert_eq!((root.as_str(), verb.as_str()), ("a", "semantic"));
    assert_root_limit(
        request_async(
            router_cache.clone(),
            workspace_args(&config_a, &root_a_path, &["semantic", "prepare"]),
            "root A excess semantic preparation",
        ),
        "test-maintenance",
        2,
        2,
    );
    assert_root_limit(
        request_async(
            router_cache.clone(),
            workspace_args(&config_b, &root_b_path, &["semantic", "prepare"]),
            "overlapping workspace semantic preparation",
        ),
        "test-maintenance",
        2,
        2,
    );

    let root_b_maintenance = request_async(
        router_cache.clone(),
        direct_args(&root_b_path, &["semantic", "prepare"]),
        "disjoint root B semantic preparation",
    );
    assert!(
        root_b_maintenance
            .wait()
            .unwrap()
            .unwrap()
            .starts_with("root-b:"),
        "root B maintenance work waited behind root A"
    );
    let (root, verb, _) = forwarded_rx.recv_timeout(TEST_WAIT).unwrap();
    assert_eq!((root.as_str(), verb.as_str()), ("b", "semantic"));

    let mut query_requests = Vec::new();
    for (root, label) in [(&root_a_path, "root A"), (&root_b_path, "root B")] {
        for words in [
            vec!["status"],
            vec!["show", "file.rs"],
            vec!["hist", "HEAD"],
            vec!["blame", "file.rs"],
            vec!["doctor"],
        ] {
            let query_label = match words[0] {
                "status" => "status",
                "show" => "show",
                "hist" => "history",
                "blame" => "blame",
                _ => "doctor",
            };
            query_requests.push(request_async(
                router_cache.clone(),
                direct_args(root, &words),
                if label == "root A" {
                    match query_label {
                        "status" => "root A status",
                        "show" => "root A show",
                        "history" => "root A history",
                        "blame" => "root A blame",
                        _ => "root A doctor",
                    }
                } else {
                    match query_label {
                        "status" => "root B status",
                        "show" => "root B show",
                        "history" => "root B history",
                        "blame" => "root B blame",
                        _ => "root B doctor",
                    }
                },
            ));
        }
    }
    for (index, query) in query_requests.into_iter().enumerate() {
        let output = query.wait().unwrap().unwrap();
        let expected_root = if index < 5 { "root-a:" } else { "root-b:" };
        assert!(
            output.starts_with(expected_root),
            "query request was not routed to its selected root: {output}"
        );
    }

    assert_eq!(root_a_counts.regex_active.load(Ordering::Acquire), 2);
    assert_eq!(root_a_counts.regex_max.load(Ordering::Acquire), 2);
    assert_eq!(root_a_counts.regex_calls.load(Ordering::Acquire), 2);
    assert_eq!(root_a_counts.maintenance_active.load(Ordering::Acquire), 1);
    assert_eq!(root_a_counts.maintenance_max.load(Ordering::Acquire), 1);
    release.release();

    for request in regex_active {
        assert!(request.wait().unwrap().unwrap().starts_with("root-a:"));
    }
    assert!(
        regex_replacement
            .wait()
            .unwrap()
            .unwrap()
            .starts_with("root-a:")
    );
    assert!(
        maintenance_active
            .wait()
            .unwrap()
            .unwrap()
            .starts_with("root-a:")
    );
    assert!(
        maintenance_queued
            .wait()
            .unwrap()
            .unwrap()
            .starts_with("root-a:")
    );
    assert_eq!(root_a_counts.regex_calls.load(Ordering::Acquire), 3);
    assert_eq!(root_a_counts.maintenance_calls.load(Ordering::Acquire), 2);

    assert!(
        request_async(
            router_cache,
            direct_args(&root_a_path, &["search", "re:after-release"]),
            "root A regex after permit release",
        )
        .wait()
        .unwrap()
        .unwrap()
        .starts_with("root-a:"),
        "completed work kept its root reservation"
    );

    daemons.router.take().unwrap().stop().unwrap();
    daemons.root_a.take().unwrap().stop().unwrap();
    daemons.root_b.take().unwrap().stop().unwrap();
}

#[test]
fn queued_permits_drop_on_pool_refusal_and_queue_cancellation() {
    let limits = super::super::root_work_limits::RootWorkLimits::default();
    let root = PathBuf::from("/repos/queued-permit");
    let pool = RequestPool::new(
        "queued-permit-test",
        PoolSize {
            workers: 1,
            queue: 1,
        },
    )
    .unwrap();
    let (started, start_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    assert!(
        pool.submit(Box::new(move || {
            let _ = started.send(());
            let _ = release_rx.recv_timeout(Duration::from_secs(10));
        }))
        .is_ok()
    );
    start_rx.recv_timeout(TEST_WAIT).unwrap();

    let queued_permit = limits
        .try_acquire(
            AdmissionTier::Router,
            RequestLane::Regex,
            std::slice::from_ref(&root),
        )
        .unwrap();
    assert!(pool.submit(Box::new(move || drop(queued_permit))).is_ok());

    let refused_permit = limits
        .try_acquire(
            AdmissionTier::Router,
            RequestLane::Regex,
            std::slice::from_ref(&root),
        )
        .unwrap();
    let refused_job: PooledJob = Box::new(move || drop(refused_permit));
    let Err(refused_job) = pool.submit(refused_job) else {
        panic!("full bounded pool accepted more than its active and queued jobs");
    };
    drop(refused_job);

    let remaining: Vec<_> = (0..2)
        .map(|_| {
            limits
                .try_acquire(
                    AdmissionTier::Router,
                    RequestLane::Regex,
                    std::slice::from_ref(&root),
                )
                .unwrap()
        })
        .collect();
    assert!(
        limits
            .try_acquire(
                AdmissionTier::Router,
                RequestLane::Regex,
                std::slice::from_ref(&root),
            )
            .is_err(),
        "the queued request's permit disappeared before cancellation"
    );
    drop(remaining);

    pool.shutdown();
    let available_after_cancel: Vec<_> = (0..3)
        .map(|_| {
            limits
                .try_acquire(
                    AdmissionTier::Router,
                    RequestLane::Regex,
                    std::slice::from_ref(&root),
                )
                .unwrap()
        })
        .collect();
    assert_eq!(available_after_cancel.len(), 3);
    assert!(
        limits
            .try_acquire(
                AdmissionTier::Router,
                RequestLane::Regex,
                std::slice::from_ref(&root),
            )
            .is_err()
    );
    release.send(()).unwrap();
    assert!(pool.drain(TEST_WAIT));
    drop(available_after_cancel);
}
