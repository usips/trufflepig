use super::*;

#[test]
fn event_stream_rejects_a_bad_plan_filter_before_success() {
    let fixture = render_fixture();
    for query in ["plan=bogus", "plan=P0", "plan=7", "plan="] {
        let reply = render_get(&fixture, &format!("/api/v1/events?{query}"));
        assert!(reply.starts_with("HTTP/1.1 400 "), "{query}: {reply}");
        assert!(
            reply.contains("\"code\":\"invalid_options\""),
            "{query}: {reply}"
        );
    }
}

#[test]
fn event_stream_capacity_refusal_answers_503_with_retry_after() {
    let fixture = render_fixture();
    let permits: Vec<_> = (0..crate::board::board_web::event_stream::STREAM_LIMIT)
        .map(|_| fixture.state.streams.reserve().unwrap())
        .collect();
    let reply = render_get(&fixture, "/api/v1/events");
    assert!(reply.starts_with("HTTP/1.1 503 "), "{reply}");
    assert!(reply.contains("Retry-After: 1\r\n"), "{reply}");
    drop(permits);
}

#[test]
fn refused_event_stream_spawn_answers_503_instead_of_silence() {
    let fixture = render_fixture();
    let poller = SequencePoller::start(Arc::new(|| Ok(EventSeq::new(0)))).unwrap();
    let foreign = EventStreams::new(
        Arc::new(|_, _, _| unreachable!("no feed reads")),
        poller.handle(),
    );
    let permit = foreign.reserve().unwrap();
    let request = StreamRequest::parse(None, None, None).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    stream_subscription(server, Ok((request, permit)), &fixture.state.streams);
    let mut reply = String::new();
    client.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 503 "), "{reply}");
    assert!(reply.contains("Retry-After: 1\r\n"), "{reply}");
    assert_eq!(foreign.active(), 0, "refused spawn released the slot");
}

#[test]
fn stopped_filler_refuses_new_subscriptions_with_503() {
    let fixture = render_fixture_with(Arc::new(|_, _, _| panic!("feed poisoned")));
    // The filler stops itself after the panicking first fill; reserve()
    // must observe the stopped ring, not just the healthy poller wake.
    let started = Instant::now();
    while fixture.state.streams.reserve().is_ok() {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "reserve() never observed the stopped filler"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        fixture.state.streams.reserve().err().unwrap().code,
        BoardErrorCode::BoardUnavailable
    );
    let reply = render_get(&fixture, "/api/v1/events");
    assert!(reply.starts_with("HTTP/1.1 503 "), "{reply}");
    assert!(reply.contains("Retry-After: 1\r\n"), "{reply}");
}

#[test]
fn event_stream_rejects_an_unknown_plan_before_success() {
    let fixture = render_fixture_with(Arc::new(|_, plan, _| match plan {
        Some(plan) => Err(BoardError::new(
            BoardErrorCode::InvalidReference,
            format!("unknown plan {plan}"),
        )),
        None => Ok(ReplayBatch {
            latest: EventSeq::new(0),
            events: vec![],
        }),
    }));
    let reply = render_get(&fixture, "/api/v1/events?plan=P99999");
    assert!(reply.starts_with("HTTP/1.1 404 "), "{reply}");
    assert!(reply.contains("\"code\":\"invalid_reference\""), "{reply}");
    assert!(!reply.contains("HTTP/1.1 200"), "{reply}");
}

#[test]
fn completed_ingest_relay_reaches_stream_subscribers() {
    let fixture = render_fixture_with(Arc::new(|_, _, _| {
        Ok(ReplayBatch {
            latest: EventSeq::new(0),
            events: vec![],
        })
    }));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut subscriber = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    subscriber
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let (server, _) = listener.accept().unwrap();
    fixture
        .state
        .streams
        .spawn(
            server,
            StreamRequest::parse(None, None, None).unwrap(),
            fixture.state.streams.reserve().unwrap(),
        )
        .unwrap();
    read_until(&mut subscriber, "\r\n\r\n");
    let reply = authed_post(
        &fixture,
        "/api/v1/ingest",
        &serde_json::json!({"api": BOARD_API}),
    );
    assert!(reply.starts_with("HTTP/1.1 202 "), "{reply}");
    // The fixture has no router, so the relay publishes its failure receipt.
    let frame = read_until(&mut subscriber, "event: ingest\n");
    assert!(frame.contains("\"error\""), "{frame}");
    assert!(frame.contains("board_unavailable"), "{frame}");
}
