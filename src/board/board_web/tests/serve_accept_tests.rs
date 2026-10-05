use super::*;

#[test]
fn stopped_ring_exits_the_serve_loop_without_any_traffic() {
    let harness = accept_harness_with(Arc::new(|_, _, _| panic!("feed poisoned")));
    wait_ring_stopped(&harness);
    let empty: Vec<io::Result<TcpStream>> = Vec::new();
    let error = serve_connections(empty, &harness.state).unwrap_err();
    assert!(
        error.to_string().contains("event ring stopped"),
        "{error:#}"
    );
}

#[test]
fn stopped_ring_drops_a_pending_connection_and_exits() {
    let harness = accept_harness_with(Arc::new(|_, _, _| panic!("feed poisoned")));
    wait_ring_stopped(&harness);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    let error = serve_connections(vec![Ok::<_, io::Error>(server)], &harness.state).unwrap_err();
    assert!(
        error.to_string().contains("event ring stopped"),
        "{error:#}"
    );
    // The restart is the recovery: the dropped connection gets no 503.
    let mut reply = String::new();
    client.read_to_string(&mut reply).unwrap();
    assert!(reply.is_empty(), "{reply}");
}

#[test]
fn healthy_ring_serves_connections_normally() {
    let harness = accept_harness_with(Arc::new(|_, _, _| {
        Ok(event_stream::ReplayBatch {
            latest: EventSeq::new(0),
            events: vec![],
        })
    }));
    assert!(harness.state.streams.reserve().is_ok());
    let empty: Vec<io::Result<TcpStream>> = Vec::new();
    serve_connections(empty, &harness.state).unwrap();
}

#[test]
fn drain_admission_stops_at_the_cap_and_reopens_on_release() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let active = AtomicUsize::new(0);
    for _ in 0..REFUSAL_DRAIN_LIMIT {
        assert!(try_admit_drain(&active, REFUSAL_DRAIN_LIMIT));
    }
    assert!(!try_admit_drain(&active, REFUSAL_DRAIN_LIMIT));
    active.fetch_sub(1, Ordering::AcqRel);
    assert!(try_admit_drain(&active, REFUSAL_DRAIN_LIMIT));
}

#[test]
fn queue_full_flood_caps_concurrent_drain_threads() {
    reset_refusal_drain_max();
    let busy =
        http_wire::unavailable_response(1, crate::board::board_web::web_serve::QUEUE_FULL_BODY);
    // Held clients keep every admitted drain inside its read so all
    // refusals overlap; without a cap the peak equals the flood size.
    let mut held = Vec::new();
    for _ in 0..96 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        client
            .write_all(b"GET /api/v1/board HTTP/1.1\r\nHost: x")
            .unwrap();
        refuse_queue_full(server, &busy);
        held.push(client);
    }
    let peak = refusal_drain_max();
    assert!(
        peak > 1,
        "the flood overlapped too little to prove the cap: {peak}"
    );
    assert!(
        peak <= REFUSAL_DRAIN_LIMIT,
        "drain threads peaked at {peak} without a cap"
    );
    drop(held);
}

#[test]
fn deferred_cleanup_removes_only_the_published_descriptor() {
    let directory = crate::board::board_test_support::scratch("web-deferred-cleanup-");
    let descriptor = directory.path().join("board-web.json");
    let address: std::net::SocketAddr = "127.0.0.1:1".parse().unwrap();
    let write_descriptor = || {
        std::fs::write(
            &descriptor,
            serde_json::json!({
                "api": BOARD_API,
                "address": address,
                "database": directory.path().join("web.sqlite3"),
            })
            .to_string(),
        )
        .unwrap();
    };
    // A signal during early bind finds nothing published: no cleanup.
    write_descriptor();
    remove_published_endpoint(&OnceLock::new());
    assert!(descriptor.exists(), "an unset lock removes nothing");
    // A foreign rewrite after publish is left alone.
    let foreign: PublishedEndpoint = OnceLock::new();
    let _ = foreign.set((descriptor.clone(), "127.0.0.1:2".parse().unwrap()));
    remove_published_endpoint(&foreign);
    assert!(descriptor.exists(), "a mismatched address removes nothing");
    // The published descriptor is removed.
    let published: PublishedEndpoint = OnceLock::new();
    let _ = published.set((descriptor.clone(), address));
    remove_published_endpoint(&published);
    assert!(!descriptor.exists(), "the published descriptor is removed");
}

#[test]
fn accept_error_classification_retries_only_transient_failures() {
    for raw in [
        libc::ECONNABORTED,
        libc::ENFILE,
        libc::EMFILE,
        libc::ENOBUFS,
        libc::ENOMEM,
    ] {
        assert!(
            transient_accept_error(&io::Error::from_raw_os_error(raw)),
            "{raw}"
        );
    }
    for kind in [
        io::ErrorKind::Interrupted,
        io::ErrorKind::ConnectionAborted,
        io::ErrorKind::WouldBlock,
        io::ErrorKind::TimedOut,
    ] {
        assert!(
            transient_accept_error(&io::Error::new(kind, "transient")),
            "{kind:?}"
        );
    }
    for kind in [
        io::ErrorKind::PermissionDenied,
        io::ErrorKind::InvalidInput,
        io::ErrorKind::AddrInUse,
    ] {
        assert!(
            !transient_accept_error(&io::Error::new(kind, "fatal")),
            "{kind:?}"
        );
    }
}

#[test]
fn transient_accept_errors_do_not_stop_the_server() {
    // The harness holds the poller like the server does; a dropped
    // poller would stop the filler and the ring it serves.
    let harness = accept_harness_with(Arc::new(|_, _, _| {
        Ok(event_stream::ReplayBatch {
            latest: EventSeq::new(0),
            events: vec![],
        })
    }));
    let state = &harness.state;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    client.write_all(b"GARBAGE\r\n\r\n").unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    let (tx, rx) = mpsc::channel::<io::Result<TcpStream>>();
    tx.send(Err(io::Error::from_raw_os_error(libc::ECONNABORTED)))
        .unwrap();
    tx.send(Err(io::Error::from_raw_os_error(libc::ENFILE)))
        .unwrap();
    tx.send(Ok(server)).unwrap();
    let worker = {
        let state = Arc::clone(state);
        std::thread::spawn(move || serve_connections(rx.into_iter(), &state))
    };
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reply = String::new();
    client.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 400 "), "{reply}");
    tx.send(Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "fatal",
    )))
    .unwrap();
    let error = worker.join().unwrap().unwrap_err();
    assert_eq!(
        error.downcast_ref::<io::Error>().map(io::Error::kind),
        Some(io::ErrorKind::PermissionDenied)
    );
}
