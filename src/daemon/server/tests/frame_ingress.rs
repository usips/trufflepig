use super::super::ingress_frames::{FinishedFrame, FrameFailure, FramePoll, PendingFrame};
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
    assert_eq!(
        pending.poll(),
        FramePoll::Idle,
        "an empty nonblocking read is an internal pending state"
    );

    client.write_all(&[0, 0]).unwrap();
    assert_eq!(pending.poll(), FramePoll::Progressed);
    std::thread::sleep(Duration::from_millis(100));
    client.write_all(&[0, 5]).unwrap();
    assert_eq!(pending.poll(), FramePoll::Progressed);
    client.write_all(&[1]).unwrap();
    assert_eq!(pending.poll(), FramePoll::Progressed);
    std::thread::sleep(Duration::from_millis(100));
    client.write_all(&[2]).unwrap();
    assert_eq!(pending.poll(), FramePoll::Progressed);
    std::thread::sleep(Duration::from_millis(350));
    assert_eq!(
        pending.poll(),
        FramePoll::Finished,
        "payload progress must not reset the frame budget"
    );
    let FinishedFrame::Failed(_, FrameFailure::TimedOut(error)) = pending.finish() else {
        panic!("expired partial frame did not carry a typed timeout");
    };
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert_eq!(error.to_string(), "timed_out: phase=reading_frame");
}

#[test]
fn header_and_payload_in_one_write_complete_in_one_poll() {
    let (server, mut client) = UnixStream::pair().unwrap();
    let mut pending = PendingFrame::new(
        server,
        QueryDeadline::after(Duration::from_secs(2)),
        FRAME_TIMEOUT,
    )
    .unwrap();
    let request = DaemonRequest::Arguments {
        context: RequestContext::new(None, None),
        args: vec!["status".to_owned()],
    };
    let mut frame = Vec::new();
    protocol::write_request(&mut frame, &request).unwrap();
    client.write_all(&frame).unwrap();
    assert_eq!(
        pending.poll(),
        FramePoll::Finished,
        "a frame already buffered must finish within one poll, not one read per turn"
    );
    assert!(matches!(pending.finish(), FinishedFrame::Complete(_)));
}

#[test]
fn sequential_round_trips_are_not_paced_by_the_accept_poll() {
    let served = Served::start(
        false,
        WIDE,
        TestDaemon {
            reply: echo(),
            reconcile: quiet(),
        },
    );
    let mut elapsed = Vec::with_capacity(50);
    for _ in 0..50 {
        let started = Instant::now();
        assert_eq!(served.ask(&["status"]).unwrap().as_deref(), Some("status"));
        elapsed.push(started.elapsed());
    }
    elapsed.sort_unstable();
    let median = elapsed[elapsed.len() / 2];
    assert!(
        median < Duration::from_millis(5),
        "median round trip {median:?} exceeds the ingress budget; slowest {:?}",
        elapsed.last().unwrap()
    );
    served.stop().unwrap();
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
    assert_eq!(pending.poll(), FramePoll::Finished);
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

#[test]
fn refused_peer_that_writes_late_still_reads_daemon_busy() {
    let served = Served::start(
        false,
        WIDE,
        TestDaemon {
            reply: echo(),
            reconcile: quiet(),
        },
    );
    let mut peers = Vec::with_capacity(64);
    for _ in 0..64 {
        peers.push(UnixStream::connect(served.cache.join(SOCKET_NAME)).unwrap());
    }

    let mut overflow = UnixStream::connect(served.cache.join(SOCKET_NAME)).unwrap();
    overflow
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let request = DaemonRequest::Arguments {
        context: RequestContext::new(None, None),
        args: vec!["status".to_owned()],
    };
    let mut frame = Vec::new();
    protocol::write_request(&mut frame, &request).unwrap();
    overflow
        .write_all(&frame)
        .expect("a refused peer's late request must not meet a closed socket");
    let DaemonReply::Failure { message } = protocol::read_reply(&mut overflow).unwrap() else {
        panic!("the refused peer must read its daemon_busy refusal");
    };
    assert!(message.starts_with("daemon_busy:"), "{message}");

    drop(overflow);
    drop(peers);
    // Lets the accept loop release the closed peers' slots before the stop connects.
    std::thread::sleep(Duration::from_millis(100));
    served.stop().unwrap();
}

#[test]
fn stop_finishes_a_frame_already_being_read() {
    let mut served = Served::start(
        false,
        WIDE,
        TestDaemon {
            reply: echo(),
            reconcile: quiet(),
        },
    );
    let request = DaemonRequest::Arguments {
        context: RequestContext::new(None, None),
        args: vec!["status".to_owned()],
    };
    let mut frame = Vec::new();
    protocol::write_request(&mut frame, &request).unwrap();
    let split = 6;
    let mut partial = UnixStream::connect(served.cache.join(SOCKET_NAME)).unwrap();
    partial
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    partial.write_all(&frame[..split]).unwrap();
    std::thread::sleep(Duration::from_millis(200));

    stop(&served.cache).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    partial
        .write_all(&frame[split..])
        .expect("stop must keep reading a frame it already accepted");
    let DaemonReply::Success { output } = protocol::read_reply(&mut partial).unwrap() else {
        panic!("the accepted frame must receive its own answer during stop");
    };
    assert_eq!(output, "status");
    served.thread.take().unwrap().join().unwrap().unwrap();
}
