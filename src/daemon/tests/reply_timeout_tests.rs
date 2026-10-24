use super::*;
use crate::board::board_protocol::BoardErrorCode;
use crate::coded_error::CodedContext;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Instant;

mod legacy_wire_tests;

const SHORT_REPLY_WAIT: Duration = Duration::from_millis(200);

fn arguments(words: &[&str]) -> DaemonRequest {
    DaemonRequest::Arguments {
        context: crate::request_context::RequestContext::new(None, None),
        args: words.iter().map(|word| (*word).to_owned()).collect(),
    }
}

fn frame(payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(4 + payload.len());
    bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

fn encoded_reply(reply: &DaemonReply) -> Vec<u8> {
    let mut bytes = Vec::new();
    protocol::write_reply(&mut bytes, reply).unwrap();
    bytes
}

fn socket_exchange(
    request: DaemonRequest,
    response: Vec<u8>,
    keep_open: bool,
    deadline: QueryDeadline,
) -> (anyhow::Result<Option<String>>, DaemonRequest, usize) {
    let scratch = scratch();
    let listener = UnixListener::bind(scratch.path().join(SOCKET_NAME)).unwrap();
    let (observed_tx, observed_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let dispatches = Arc::new(AtomicUsize::new(0));
    let peer_dispatches = Arc::clone(&dispatches);
    let peer = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let observed = protocol::read_request(&mut stream).unwrap();
        peer_dispatches.fetch_add(1, Ordering::SeqCst);
        observed_tx.send(observed).unwrap();
        stream.write_all(&response).unwrap();
        if keep_open {
            release_rx.recv().unwrap();
        }
    });
    let cache = scratch.path().to_owned();
    let client = std::thread::spawn(move || super::super::exchange(&cache, &request, deadline));
    let observed = observed_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("daemon should observe the request");
    let result = client.join().unwrap();
    let _ = release_tx.send(());
    peer.join().unwrap();
    (result, observed, dispatches.load(Ordering::SeqCst))
}

fn socket_exchange_fragmented(
    request: DaemonRequest,
    response: Vec<u8>,
    delay: Duration,
    deadline: QueryDeadline,
) -> anyhow::Result<Option<String>> {
    let scratch = scratch();
    let listener = UnixListener::bind(scratch.path().join(SOCKET_NAME)).unwrap();
    let peer = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        protocol::read_request(&mut stream).unwrap();
        stream.write_all(&response[..2]).unwrap();
        std::thread::sleep(delay);
        stream.write_all(&response[2..]).unwrap();
    });
    let result = super::super::exchange(scratch.path(), &request, deadline);
    peer.join().unwrap();
    result
}

fn exchange_cancelled(deadline: QueryDeadline, cancel: impl FnOnce()) -> anyhow::Error {
    let scratch = scratch();
    let listener = UnixListener::bind(scratch.path().join(SOCKET_NAME)).unwrap();
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (eof_tx, eof_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let peer = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        protocol::read_request(&mut stream).unwrap();
        accepted_tx.send(()).unwrap();
        let mut byte = [0_u8; 1];
        eof_tx.send(stream.read(&mut byte).unwrap()).unwrap();
        release_rx.recv().unwrap();
    });
    let cache = scratch.path().to_owned();
    let request = arguments(&["show", "status"]);
    let (result_tx, result_rx) = mpsc::channel();
    let client = std::thread::spawn(move || {
        result_tx
            .send(super::super::exchange(&cache, &request, deadline))
            .unwrap();
    });
    accepted_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("daemon should accept the request before cancellation");
    let cancelled_at = Instant::now();
    cancel();
    let error = result_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("cancellation should interrupt the downstream reply wait")
        .unwrap_err();
    let timeout = assert_timeout(&error, Duration::from_secs(5));
    assert!(timeout.elapsed_wait() < Duration::from_secs(2));
    assert_eq!(
        eof_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("cancelled exchange should close its daemon socket promptly"),
        0,
        "daemon should observe EOF after the ambiguous timeout"
    );
    assert!(cancelled_at.elapsed() < Duration::from_secs(2));
    release_tx.send(()).unwrap();
    peer.join().unwrap();
    client.join().unwrap();
    error
}

fn assert_timeout(error: &anyhow::Error, maximum_budget: Duration) -> &DaemonReplyTimeout {
    let timeout = DaemonReplyTimeout::from_error(error)
        .unwrap_or_else(|| panic!("expected typed reply timeout, got {error:#}"));
    assert_eq!(BoardErrorCode::from_error(error), None);
    assert_eq!(timeout.retry_after(), Duration::from_millis(250));
    assert!(timeout.wait_budget() > Duration::ZERO);
    assert!(timeout.wait_budget() <= maximum_budget);
    let display = timeout.to_string();
    assert!(
        display.starts_with("daemon_reply_timeout: phase=awaiting_reply wait_ms="),
        "{display}"
    );
    assert!(
        display.ends_with("retry_after_ms=250 replay=read_only"),
        "{display}"
    );
    timeout
}

#[test]
fn daemon_reply_timeout_maps_partial_header_read_timeout() {
    let (result, _, _) = socket_exchange(
        arguments(&["show", "status"]),
        vec![0, 0],
        true,
        QueryDeadline::after(SHORT_REPLY_WAIT),
    );
    let error = result.unwrap_err();
    assert_timeout(&error, SHORT_REPLY_WAIT);
}

#[test]
fn daemon_reply_timeout_maps_partial_payload_read_timeout() {
    let (result, _, _) = socket_exchange(
        arguments(&["show", "status"]),
        vec![0, 0, 0, 8, b'{'],
        true,
        QueryDeadline::after(SHORT_REPLY_WAIT),
    );
    assert_timeout(&result.unwrap_err(), SHORT_REPLY_WAIT);
}

#[test]
fn daemon_reply_timeout_after_accepted_board_write_is_not_daemon_busy() {
    let request = arguments(&["board", "post", "P11.2", "progress", "accepted"]);
    let (result, observed, dispatches) = socket_exchange(
        request,
        vec![0, 0],
        true,
        QueryDeadline::after(SHORT_REPLY_WAIT),
    );
    let DaemonRequest::Arguments { args, .. } = observed else {
        panic!("expected accepted argument request");
    };
    assert_eq!(args[0], "board");
    assert_eq!(args[1], "post");
    let error = result.unwrap_err();
    assert_timeout(&error, SHORT_REPLY_WAIT);
    assert_eq!(dispatches, 1);
    assert_ne!(
        BoardErrorCode::from_error(&error),
        Some(BoardErrorCode::DaemonBusy)
    );
}

#[test]
fn daemon_reply_timeout_zero_initial_budget_never_connects() {
    let scratch = scratch();
    let listener = UnixListener::bind(scratch.path().join(SOCKET_NAME)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let error = super::super::exchange(
        scratch.path(),
        &arguments(&["board", "post"]),
        QueryDeadline::after(Duration::ZERO),
    )
    .unwrap_err();
    assert!(DaemonReplyTimeout::from_error(&error).is_none());
    assert!(matches!(
        listener.accept(),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
}

#[test]
fn daemon_reply_timeout_keeps_truncated_and_decode_errors_distinct() {
    let (truncated, _, _) = socket_exchange(
        arguments(&["show", "status"]),
        vec![0, 0],
        false,
        QueryDeadline::after(SHORT_REPLY_WAIT),
    );
    let truncated = truncated.unwrap_err();
    assert!(DaemonReplyTimeout::from_error(&truncated).is_none());
    assert!(truncated.to_string().contains("read daemon frame length"));

    let (decode, _, _) = socket_exchange(
        arguments(&["show", "status"]),
        frame(b"?"),
        false,
        QueryDeadline::after(SHORT_REPLY_WAIT),
    );
    let decode = decode.unwrap_err();
    assert!(DaemonReplyTimeout::from_error(&decode).is_none());
    assert!(decode.to_string().contains("decode daemon response"));
}

#[test]
fn daemon_reply_timeout_round_trips_through_socket_and_spool() {
    let expected = DaemonReplyTimeout::new(Duration::from_millis(37), CLIENT_REPLY_WAIT);
    let forwarded = anyhow::Error::new(expected.clone()).context("forwarded reply timeout");
    let reply = DaemonReply::from_result(Err(forwarded));
    let DaemonReply::Failure { message } = &reply else {
        panic!("typed timeout should use the failure wire variant");
    };
    assert_eq!(message, &expected.to_string());

    let (socket_result, _, _) = socket_exchange(
        arguments(&["show", "status"]),
        encoded_reply(&reply),
        false,
        QueryDeadline::after(CLIENT_REPLY_WAIT),
    );
    let socket_error = socket_result.unwrap_err();
    assert_eq!(
        DaemonReplyTimeout::from_error(&socket_error),
        Some(&expected)
    );

    let scratch = scratch();
    let dir = scratch.path().join("spool");
    let _server = spool::SpoolServer::open(&dir).unwrap();
    let context = crate::request_context::RequestContext::new(None, None);
    let reply_path = dir.join(format!("{}.reply", context.request_id));
    fs::write(&reply_path, serde_json::to_vec(&reply).unwrap()).unwrap();
    let spool_error = spool::request(
        &dir,
        &["show".to_owned(), "status".to_owned()],
        &context,
        QueryDeadline::after(CLIENT_REPLY_WAIT),
    )
    .unwrap_err();
    assert_eq!(
        DaemonReplyTimeout::from_error(&spool_error),
        Some(&expected)
    );
}

#[test]
fn terminal_timed_out_context_round_trips_without_timeout_replay() {
    let expected = DaemonReplyTimeout::new(Duration::from_millis(37), CLIENT_REPLY_WAIT);
    let outer_context = "timed_out: daemon query budget expired";
    let error = anyhow::Error::new(expected.clone())
        .with_coded_context(|| coded!(TimedOut, "daemon query budget expired"));
    let expected_message = format!("{outer_context}: {expected}");
    let reply = DaemonReply::from_result(Err(error));
    let DaemonReply::Failure { message } = &reply else {
        panic!("terminal timeout should use the failure wire variant");
    };
    assert_eq!(message, &expected_message);

    let (socket_result, _, _) = socket_exchange(
        arguments(&["show", "status"]),
        encoded_reply(&reply),
        false,
        QueryDeadline::after(CLIENT_REPLY_WAIT),
    );
    let socket_error = socket_result.unwrap_err();
    assert_eq!(socket_error.to_string(), expected_message);
    assert!(socket_error.to_string().contains(&expected.to_string()));
    assert!(DaemonReplyTimeout::from_error(&socket_error).is_none());

    let scratch = scratch();
    let dir = scratch.path().join("spool");
    let _server = spool::SpoolServer::open(&dir).unwrap();
    let context = crate::request_context::RequestContext::new(None, None);
    let reply_path = dir.join(format!("{}.reply", context.request_id));
    fs::write(&reply_path, serde_json::to_vec(&reply).unwrap()).unwrap();
    let spool_error = spool::request(
        &dir,
        &["show".to_owned(), "status".to_owned()],
        &context,
        QueryDeadline::after(CLIENT_REPLY_WAIT),
    )
    .unwrap_err();
    assert_eq!(spool_error.to_string(), expected_message);
    assert!(spool_error.to_string().contains(&expected.to_string()));
    assert!(DaemonReplyTimeout::from_error(&spool_error).is_none());
}

#[test]
fn malformed_reply_timeout_failure_prefix_stays_an_ordinary_failure() {
    let messages = [
        "daemon_reply_timeout: phase=reading_reply wait_ms=37 deadline_ms=30000 retry_after_ms=250 replay=read_only",
        "daemon_reply_timeout: phase=awaiting_reply wait_ms=soon deadline_ms=30000 retry_after_ms=250 replay=read_only",
        "daemon_reply_timeout: phase=awaiting_reply wait_ms=+37 deadline_ms=30000 retry_after_ms=250 replay=read_only",
        "daemon_reply_timeout: phase=awaiting_reply wait_ms=37 retry_after_ms=250 replay=read_only",
        "daemon_reply_timeout: phase=awaiting_reply wait_ms=037 deadline_ms=30000 retry_after_ms=250 replay=read_only",
        "daemon_reply_timeout: phase=awaiting_reply wait_ms=18446744073709551616 deadline_ms=30000 retry_after_ms=250 replay=read_only",
        "daemon_reply_timeout: phase=awaiting_reply wait_ms=37 deadline_ms=30000 retry_after_ms=250 replay=write",
        "daemon_reply_timeout: phase=awaiting_reply wait_ms=37 deadline_ms=30000 retry_after_ms=250 replay=read_only extra",
    ];

    for message in messages {
        let error = DaemonReply::Failure {
            message: message.to_owned(),
        }
        .into_result()
        .unwrap_err();
        assert!(
            DaemonReplyTimeout::from_error(&error).is_none(),
            "{message}"
        );
        assert_eq!(error.to_string(), format!("daemon: {message}"));
    }

    let error = DaemonReply::Failure {
        message: "timed_out".to_owned(),
    }
    .into_result()
    .unwrap_err();
    assert!(DaemonReplyTimeout::from_error(&error).is_none());
    assert_eq!(error.to_string(), "daemon: timed_out");
}

#[test]
fn ordinary_failure_message_keeps_its_existing_display() {
    let reply = DaemonReply::from_result(Err(anyhow::anyhow!("handler failed").context("request")));
    let DaemonReply::Failure { message } = reply else {
        panic!("ordinary failure should use failure status");
    };
    assert_eq!(message, "request: handler failed");

    let error = DaemonReply::Failure { message }.into_result().unwrap_err();
    assert_eq!(error.to_string(), "daemon: request: handler failed");
    assert!(DaemonReplyTimeout::from_error(&error).is_none());
}

#[test]
fn daemon_reply_timeout_cancels_promptly_when_peer_closes() {
    let (cancel_peer, cancel_handle) = UnixStream::pair().unwrap();
    let deadline = QueryDeadline::after(Duration::from_secs(5)).with_peer(Arc::new(cancel_peer));
    let error = exchange_cancelled(deadline, move || drop(cancel_handle));
    assert_timeout(&error, Duration::from_secs(5));
}

#[test]
fn daemon_reply_timeout_cancels_when_spool_claim_is_withdrawn() {
    let scratch = scratch();
    let claim_path = scratch.path().join("request.claimed");
    fs::write(&claim_path, b"queued request").unwrap();
    let deadline = QueryDeadline::after(Duration::from_secs(5)).with_claim_path(claim_path.clone());
    let error = exchange_cancelled(deadline, move || fs::remove_file(claim_path).unwrap());
    assert_timeout(&error, Duration::from_secs(5));
}

#[test]
fn daemon_reply_timeout_slices_keep_fragmented_frame_progress() {
    let reply = DaemonReply::Success {
        output: "fragmented response".to_owned(),
    };
    let result = socket_exchange_fragmented(
        arguments(&["show", "status"]),
        encoded_reply(&reply),
        Duration::from_millis(150),
        QueryDeadline::after(Duration::from_secs(2)),
    );
    assert_eq!(result.unwrap().as_deref(), Some("fragmented response"));
}

#[test]
fn daemon_spool_timeout_removes_claimed_request_and_preserves_reply() {
    let scratch = scratch();
    let dir = scratch.path().join("spool");
    let _server = spool::SpoolServer::open(&dir).unwrap();
    let context = crate::request_context::RequestContext::new(None, None);
    let request_path = dir.join(format!("{}.request", context.request_id));
    let claimed_path = dir.join(format!("{}.claimed", context.request_id));
    let reply_path = dir.join(format!("{}.reply", context.request_id));
    fs::write(&claimed_path, b"queued claim").unwrap();
    fs::write(&reply_path, b"preserve result").unwrap();

    spool::expire_after_next_publication();
    let error = spool::request(
        &dir,
        &["board".to_owned(), "post".to_owned()],
        &context,
        QueryDeadline::after(Duration::from_secs(2)),
    )
    .unwrap_err();

    assert!(spool::is_local_spool_timeout(&error));
    assert!(spool::publication_expired());
    assert!(!request_path.exists());
    assert!(!claimed_path.exists());
    assert_eq!(fs::read(reply_path).unwrap(), b"preserve result");
}
