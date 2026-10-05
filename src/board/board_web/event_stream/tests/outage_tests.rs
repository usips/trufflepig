use super::*;

#[test]
fn quiet_disconnect_releases_the_stream_slot() {
    let streams = streams(empty_reader());
    let mut client = spawn(&streams, 0);
    read_until(&mut client, "\r\n\r\n");
    drop(client);
    wait_released(&streams);
}

#[test]
fn failing_feeds_close_streams_and_panicking_feeds_refuse_reserve() {
    let down_feed = FakeFeed::seeded(vec![]);
    down_feed.down.store(true, Ordering::Release);
    let failing = streams(down_feed.reader());
    let mut client = spawn(&failing, 0);
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    wait_released(&failing);
    let panicking = Arc::new(|_, _, _| -> Result<ReplayBatch, BoardError> { panic!("feed panic") })
        as FeedReader;
    let panicked = streams(panicking);
    // A poisoned feed stops the ring, so reserve() refuses new streams
    // instead of spawning closes; no slot is held or leaked.
    let started = Instant::now();
    while panicked.reserve().is_ok() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "reserve() never observed the stopped filler"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        panicked.reserve().err().unwrap().code,
        BoardErrorCode::BoardUnavailable
    );
    assert_eq!(panicked.active(), 0);
}

#[test]
fn recovering_feed_restores_availability_and_ordered_replay() {
    let feed = FakeFeed::seeded(vec![event(1, "one"), event(2, "two")]);
    let wake = SequenceWake::new();
    let streams = EventStreams::new(feed.reader(), wake.clone());
    let mut client = spawn(&streams, 0);
    read_until(&mut client, "id: 2\n");
    let permits: Vec<_> = (1..STREAM_LIMIT)
        .map(|_| streams.reserve().unwrap())
        .collect();
    assert_eq!(streams.active(), STREAM_LIMIT);
    wake.mark_unavailable();
    assert_eq!(
        streams.reserve().err().unwrap().code,
        BoardErrorCode::BoardUnavailable
    );
    let mut remaining = String::new();
    client.read_to_string(&mut remaining).unwrap();
    drop(permits);
    wait_released(&streams);
    wake.publish(EventSeq::new(2));
    let permits: Vec<_> = (0..STREAM_LIMIT)
        .map(|_| streams.reserve().unwrap())
        .collect();
    assert_eq!(streams.active(), STREAM_LIMIT);
    drop(permits);
    let mut client = spawn(&streams, 0);
    let replay = read_until(&mut client, "id: 2\n");
    assert!(replay.find("id: 1\n").unwrap() < replay.find("id: 2\n").unwrap());
    drop(client);
    wait_released(&streams);
}

#[test]
fn outage_between_reservation_and_spawn_answers_503() {
    let wake = SequenceWake::new();
    let streams = EventStreams::new(empty_reader(), wake.clone());
    let permit = streams.reserve().unwrap();
    wake.mark_unavailable();
    let (server, mut client) = socket_pair();
    streams
        .spawn(
            server,
            StreamRequest::parse(None, None, None).unwrap(),
            permit,
        )
        .unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 503 "), "{response}");
    assert!(response.contains("Retry-After: 1\r\n"), "{response}");
    assert!(
        response.contains("\"code\":\"board_unavailable\""),
        "{response}"
    );
    assert!(!response.contains("HTTP/1.1 200"), "{response}");
    wait_released(&streams);
}

#[test]
fn dropped_poller_between_reservation_and_spawn_answers_503() {
    let poller = SequencePoller::start(Arc::new(|| Ok(EventSeq::new(0)))).unwrap();
    let streams = EventStreams::new(empty_reader(), poller.handle());
    let permit = streams.reserve().unwrap();
    drop(poller);
    let (server, mut client) = socket_pair();
    streams
        .spawn(
            server,
            StreamRequest::parse(None, None, None).unwrap(),
            permit,
        )
        .unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 503 "), "{response}");
    assert!(response.contains("Retry-After: 1\r\n"), "{response}");
    assert!(
        response.contains("\"code\":\"board_unavailable\""),
        "{response}"
    );
    assert!(!response.contains("HTTP/1.1 200"), "{response}");
    wait_released(&streams);
}
