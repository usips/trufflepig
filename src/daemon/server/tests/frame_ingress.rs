use super::super::ingress_frames::{FinishedFrame, FrameFailure, PendingFrame};
use super::*;
use std::io::Write;

#[test]
fn silent_and_partial_peers_do_not_hold_ingress_workers() {
    let served = Served::start(
        false,
        WIDE,
        TestDaemon {
            reply: echo(),
            reconcile: quiet(),
        },
    );
    let mut stalled = Vec::new();
    for _ in 0..2 {
        stalled.push(UnixStream::connect(served.cache.join(SOCKET_NAME)).unwrap());
    }
    for _ in 0..2 {
        let mut stream = UnixStream::connect(served.cache.join(SOCKET_NAME)).unwrap();
        let request = DaemonRequest::Arguments {
            context: RequestContext::new(None, None),
            args: vec!["search".to_owned(), "partial".to_owned()],
        };
        let mut frame = Vec::new();
        protocol::write_request(&mut frame, &request).unwrap();
        let split = if stalled.len() == 2 { 2 } else { 6 };
        stream.write_all(&frame[..split]).unwrap();
        stalled.push(stream);
    }

    let started = Instant::now();
    assert_eq!(served.ask(&["status"]).unwrap().as_deref(), Some("status"));
    assert!(started.elapsed() < Duration::from_secs(1));
    drop(stalled);
    served.stop().unwrap();
}

#[test]
fn would_block_is_pending_and_header_payload_share_one_absolute_budget() {
    let (server, mut client) = UnixStream::pair().unwrap();
    let mut pending = PendingFrame::new(
        server,
        QueryDeadline::after(Duration::from_secs(2)),
        Duration::from_millis(500),
    )
    .unwrap();
    assert!(
        !pending.poll(),
        "an empty nonblocking read is an internal pending state"
    );

    client.write_all(&[0, 0]).unwrap();
    assert!(!pending.poll());
    std::thread::sleep(Duration::from_millis(100));
    client.write_all(&[0, 5]).unwrap();
    assert!(!pending.poll());
    client.write_all(&[1]).unwrap();
    assert!(!pending.poll());
    std::thread::sleep(Duration::from_millis(100));
    client.write_all(&[2]).unwrap();
    assert!(!pending.poll());
    std::thread::sleep(Duration::from_millis(350));
    assert!(
        pending.poll(),
        "payload progress must not reset the frame budget"
    );
    let FinishedFrame::Failed(_, FrameFailure::TimedOut(error)) = pending.finish() else {
        panic!("expired partial frame did not carry a typed timeout");
    };
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert_eq!(error.to_string(), "timed_out: phase=reading_frame");
}

#[test]
fn oversized_length_is_rejected_before_payload_allocation() {
    let (server, mut client) = UnixStream::pair().unwrap();
    let mut pending = PendingFrame::new(
        server,
        QueryDeadline::after(Duration::from_secs(2)),
        FRAME_TIMEOUT,
    )
    .unwrap();
    client
        .write_all(&((protocol::REQUEST_LIMIT + 1) as u32).to_be_bytes())
        .unwrap();
    assert!(pending.poll());
    assert_eq!(pending.buffered_len(), 0);
    assert!(matches!(
        pending.finish(),
        FinishedFrame::Failed(_, FrameFailure::InvalidLength(_))
    ));
}

#[test]
fn expired_socket_frame_returns_typed_failure_with_short_write_grace() {
    let served = Served::start(
        false,
        WIDE,
        TestDaemon {
            reply: echo(),
            reconcile: quiet(),
        },
    );
    let mut client = UnixStream::connect(served.cache.join(SOCKET_NAME)).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(7)))
        .unwrap();
    client.write_all(&[0, 0]).unwrap();

    let DaemonReply::Failure { message } = protocol::read_reply(&mut client).unwrap() else {
        panic!("expired frame must receive a failure");
    };
    assert_eq!(message, "timed_out: phase=reading_frame");
    drop(client);
    served.stop().unwrap();
}

#[test]
fn pending_frame_capacity_is_tier_bounded() {
    assert_eq!(AdmissionTier::Root.pending_frame_capacity(), 64);
    assert_eq!(AdmissionTier::Coordinator.pending_frame_capacity(), 64);
    assert_eq!(AdmissionTier::Router.pending_frame_capacity(), 128);
}

#[test]
fn pending_frame_capacity_refuses_the_next_silent_peer_without_blocking() {
    let served = Served::start(
        false,
        WIDE,
        TestDaemon {
            reply: echo(),
            reconcile: quiet(),
        },
    );
    let mut peers = Vec::with_capacity(65);
    for _ in 0..65 {
        peers.push(UnixStream::connect(served.cache.join(SOCKET_NAME)).unwrap());
    }

    let mut overflow = peers.pop().unwrap();
    overflow
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let DaemonReply::Failure { message } = protocol::read_reply(&mut overflow).unwrap() else {
        panic!("the peer beyond pending capacity must receive a refusal");
    };
    assert!(message.starts_with("daemon_busy:"), "{message}");
    assert!(message.contains("pool=test-ingress"), "{message}");

    drop(overflow);
    drop(peers);
    served.stop().unwrap();
}
