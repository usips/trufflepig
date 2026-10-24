use super::super::{DaemonSocket, RequestAdmission, ServerState, accept_until_stopped};
use super::*;
use std::path::PathBuf;
use std::sync::mpsc;

#[test]
fn forwarding_tiers_keep_twenty_four_and_twenty_eight_second_headroom() {
    assert_eq!(
        AdmissionTier::Root.accepted_budget(),
        Duration::from_secs(20)
    );
    assert_eq!(
        AdmissionTier::Coordinator.accepted_budget(),
        Duration::from_secs(24)
    );
    assert_eq!(
        AdmissionTier::Router.accepted_budget(),
        Duration::from_secs(28)
    );
}

struct BudgetServer {
    _scratch: tempfile::TempDir,
    cache: PathBuf,
    thread: Option<std::thread::JoinHandle<Result<()>>>,
}

fn start_budget_server(tier: AdmissionTier, budget: Duration, reply: Reply) -> BudgetServer {
    let scratch = scratch();
    let root = scratch.path().join("repo");
    let cache = scratch.path().join("cache");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&cache).unwrap();
    let socket = DaemonSocket::bind(&cache).unwrap();
    let admission = Arc::new(RequestAdmission::new("budget-test", tier, WIDE).unwrap());
    let state = Arc::new(ServerState::default());
    let handler = Arc::new(TestDaemon {
        reply,
        reconcile: quiet(),
    });
    let (handler_for_thread, admission_for_thread, state_for_thread) = (
        Arc::clone(&handler),
        Arc::clone(&admission),
        Arc::clone(&state),
    );
    let thread = std::thread::spawn(move || {
        let result = accept_until_stopped(
            &socket,
            &handler_for_thread,
            &admission_for_thread,
            &state_for_thread,
            budget,
        );
        admission_for_thread.drain(Duration::from_secs(2));
        admission_for_thread.shutdown();
        result
    });
    BudgetServer {
        _scratch: scratch,
        cache,
        thread: Some(thread),
    }
}

fn query_reply(cache: PathBuf) -> Result<DaemonReply> {
    query_words(cache, &["status"])
}

fn query_words(cache: PathBuf, words: &[&str]) -> Result<DaemonReply> {
    let mut stream = UnixStream::connect(cache.join(SOCKET_NAME))?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    daemon_wire_protocol::write_request(
        &mut stream,
        &DaemonRequest::Arguments {
            context: RequestContext::new(None, None),
            args: words.iter().map(|word| (*word).to_owned()).collect(),
        },
    )?;
    Ok(daemon_wire_protocol::read_reply(&mut stream)?)
}

/// A root-tier server whose handler only succeeds after its deadline has passed.
fn late_success_server() -> BudgetServer {
    start_budget_server(
        AdmissionTier::Root,
        Duration::from_millis(300),
        Box::new(|request| {
            while !request.deadline.time_expired() {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok("late success".to_owned())
        }),
    )
}

#[test]
fn late_success_of_a_history_verb_still_reaches_the_client() {
    let mut root = late_success_server();
    let reply = query_words(root.cache.clone(), &["blame", "src/lib.rs"]).unwrap();
    assert!(
        matches!(&reply, DaemonReply::Success { output } if output == "late success"),
        "history verbs do not observe the deadline; their finished result must arrive"
    );
    stop_budget_server(&mut root);
}

#[test]
fn late_success_of_a_regex_search_becomes_timed_out() {
    let mut root = late_success_server();
    let reply = query_words(root.cache.clone(), &["search", "re:late"]).unwrap();
    let DaemonReply::Failure { message } = reply else {
        panic!("regex work honours its deadline and must not deliver a late success");
    };
    assert!(message.starts_with("timed_out:"), "{message}");
    stop_budget_server(&mut root);
}

fn stop_budget_server(server: &mut BudgetServer) {
    let mut stream = UnixStream::connect(server.cache.join(SOCKET_NAME)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    daemon_wire_protocol::write_request(&mut stream, &DaemonRequest::Stop).unwrap();
    assert!(matches!(
        daemon_wire_protocol::read_reply(&mut stream).unwrap(),
        DaemonReply::Success { .. }
    ));
    server.thread.take().unwrap().join().unwrap().unwrap();
}

#[test]
fn shorter_root_budget_expires_before_coordinator_and_router_on_socket_path() {
    let mut root = start_budget_server(
        AdmissionTier::Root,
        Duration::from_millis(400),
        Box::new(|request| {
            while !request.deadline.time_expired() {
                std::thread::sleep(Duration::from_millis(5));
            }
            anyhow::bail!(coded!(TimedOut, "scaled root deadline expired"))
        }),
    );
    let root_cache = root.cache.clone();
    let (coordinator_remaining, coordinator_rx) = mpsc::channel();
    let mut coordinator = start_budget_server(
        AdmissionTier::Coordinator,
        Duration::from_millis(900),
        Box::new(move |request| {
            std::thread::sleep(Duration::from_millis(80));
            let result = crate::daemon_transport::request_by(
                &root_cache,
                &request.args,
                &request.context,
                request.deadline.clone(),
            );
            let _ = coordinator_remaining.send(request.deadline.remaining());
            result?.ok_or_else(|| anyhow::anyhow!("root daemon disappeared"))
        }),
    );
    let coordinator_cache = coordinator.cache.clone();
    let (router_remaining, router_rx) = mpsc::channel();
    let mut router = start_budget_server(
        AdmissionTier::Router,
        Duration::from_millis(1500),
        Box::new(move |request| {
            std::thread::sleep(Duration::from_millis(80));
            let result = crate::daemon_transport::request_by(
                &coordinator_cache,
                &request.args,
                &request.context,
                request.deadline.clone(),
            );
            let _ = router_remaining.send(request.deadline.remaining());
            result?.ok_or_else(|| anyhow::anyhow!("coordinator daemon disappeared"))
        }),
    );

    let client_cache = router.cache.clone();
    let client = std::thread::spawn(move || query_reply(client_cache));
    let DaemonReply::Failure { message } = client.join().unwrap().unwrap() else {
        panic!("root work must expire before its reply");
    };
    assert!(message.starts_with("timed_out:"), "{message}");
    assert!(
        coordinator_rx.recv_timeout(Duration::from_secs(1)).unwrap() > Duration::from_millis(200),
        "coordinator must receive the root timeout with budget remaining"
    );
    assert!(
        router_rx.recv_timeout(Duration::from_secs(1)).unwrap() > Duration::from_millis(500),
        "router must receive the coordinator timeout with budget remaining"
    );

    stop_budget_server(&mut root);
    stop_budget_server(&mut coordinator);
    stop_budget_server(&mut router);
}

#[test]
fn expired_router_budget_leads_over_a_later_root_reply_timeout() {
    let (root_entered, root_entries) = mpsc::channel();
    let (root_finished, root_finishes) = mpsc::channel();
    let mut root = start_budget_server(
        AdmissionTier::Root,
        Duration::from_millis(2500),
        Box::new(move |request| {
            let _ = root_entered.send(());
            while !request.deadline.time_expired() {
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = root_finished.send(());
            Ok("late root result".to_owned())
        }),
    );
    let root_cache = root.cache.clone();
    let mut router = start_budget_server(
        AdmissionTier::Router,
        Duration::from_millis(450),
        Box::new(move |request| {
            match crate::daemon_transport::request_by(
                &root_cache,
                &request.args,
                &request.context,
                request.deadline.clone(),
            )? {
                Some(reply) => Ok(reply),
                None => anyhow::bail!("root daemon disappeared"),
            }
        }),
    );

    let started = Instant::now();
    let client_cache = router.cache.clone();
    let client = std::thread::spawn(move || query_reply(client_cache));
    root_entries
        .recv_timeout(Duration::from_secs(2))
        .expect("router did not forward the request to the root");
    let DaemonReply::Failure { message } = client.join().unwrap().unwrap() else {
        panic!("the router's own deadline must terminate before the root reply");
    };
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(
        matches!(root_finishes.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "root work should still be waiting when the router returns"
    );
    assert!(message.starts_with("timed_out:"), "{message}");
    assert!(
        message.contains("daemon_reply_timeout: phase=awaiting_reply"),
        "the downstream transport cause must remain visible: {message}"
    );

    root_finishes
        .recv_timeout(Duration::from_secs(3))
        .expect("later root work did not reach its own deadline");
    stop_budget_server(&mut root);
    stop_budget_server(&mut router);
}

#[test]
fn owned_timeout_normalization_requires_expired_bounded_work_and_typed_cause() {
    let class = RequestClass {
        lane: RequestLane::Query,
        verb: "status",
    };
    let expired = QueryDeadline::after(Duration::ZERO);
    let downstream_timeout = anyhow::Error::new(crate::daemon_transport::DaemonReplyTimeout::new(
        Duration::from_millis(40),
        Duration::from_millis(450),
    ));
    let normalized =
        super::super::normalize_owned_query_timeout(class, Err(downstream_timeout), &expired)
            .unwrap_err();
    assert!(normalized.to_string().starts_with("timed_out:"));
    assert!(crate::daemon_transport::DaemonReplyTimeout::from_error(&normalized).is_some());

    let live = QueryDeadline::after(Duration::from_secs(1));
    let downstream_timeout = anyhow::Error::new(crate::daemon_transport::DaemonReplyTimeout::new(
        Duration::from_millis(40),
        Duration::from_secs(1),
    ));
    let preserved =
        super::super::normalize_owned_query_timeout(class, Err(downstream_timeout), &live)
            .unwrap_err();
    assert!(preserved.to_string().starts_with("daemon_reply_timeout:"));

    let corruption = anyhow::anyhow!("corrupt index record");
    let preserved =
        super::super::normalize_owned_query_timeout(class, Err(corruption), &expired).unwrap_err();
    assert_eq!(preserved.to_string(), "corrupt index record");
}
