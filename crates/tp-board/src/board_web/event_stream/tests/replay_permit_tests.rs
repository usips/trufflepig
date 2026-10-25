use super::*;

#[test]
fn event_cursor_header_precedes_query_without_malformed_fallback() {
    assert_eq!(
        StreamRequest::parse(Some("9"), Some("3"), Some("P7")).unwrap(),
        StreamRequest {
            after: EventSeq::new(9),
            plan: Some(PlanId::new(7).unwrap()),
        }
    );
    assert_eq!(
        StreamRequest::parse(None, None, None).unwrap().after.get(),
        0
    );
    for value in ["", "-1", "+1", "01", "1 ", "9223372036854775808"] {
        assert_eq!(
            StreamRequest::parse(Some(value), Some("3"), None)
                .unwrap_err()
                .code,
            BoardErrorCode::InvalidOptions
        );
    }
    assert!(StreamRequest::parse(None, Some("3"), Some("7")).is_err());
}

#[test]
fn stream_permits_cap_capacity_and_recover_on_drop_and_unwind() {
    let streams = streams(empty_reader());
    let mut permits: Vec<_> = (0..STREAM_LIMIT)
        .map(|_| streams.reserve().unwrap())
        .collect();
    assert_eq!(
        streams.reserve().err().unwrap().code,
        BoardErrorCode::DaemonBusy
    );
    drop(permits.pop());
    let permit = streams.reserve().unwrap();
    assert!(
        std::panic::catch_unwind(move || {
            let _permit = permit;
            panic!("subscriber panic");
        })
        .is_err()
    );
    assert_eq!(streams.active(), STREAM_LIMIT - 1);
    drop(permits);
    assert_eq!(streams.active(), 0);
}

#[test]
fn replay_precedes_wait_and_serializes_multiline_utf8_json() {
    let feed = FakeFeed::seeded(vec![event(5, "雪\n\"quoted\""), event(8, "next")]);
    let streams = streams(feed.reader());
    let mut client = spawn(&streams, 4);
    let response = read_until(&mut client, "id: 8\n");
    assert!(response.contains("Connection: close\r\n"));
    assert!(response.contains("Cache-Control: no-store\r\n"));
    assert!(!response.contains("Content-Length:"));
    assert!(response.find("id: 5\n").unwrap() < response.find("id: 8\n").unwrap());
    let data = response
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .unwrap();
    let decoded: EventRecord = serde_json::from_str(data).unwrap();
    assert_eq!(decoded.summary.as_str(), "雪\n\"quoted\"");
    // An idle subscriber drives no reads: the count settles instead of
    // merely surviving a fixed window that load can shift.
    wait_reads_quiet(|| feed.read_count(), Duration::from_millis(300));
    drop(client);
    wait_released(&streams);
}

#[test]
fn two_subscribers_replay_one_shared_fill() {
    let feed = FakeFeed::seeded(vec![event(1, "one"), event(2, "two"), event(3, "three")]);
    let wake = SequenceWake::new();
    let streams = EventStreams::new(feed.reader(), wake.clone());
    let mut first = spawn(&streams, 0);
    let mut second = spawn(&streams, 0);
    let replay_a = read_until(&mut first, "id: 3\n");
    let replay_b = read_until(&mut second, "id: 3\n");
    let payload_a = replay_a.split("\r\n\r\n").nth(1).unwrap();
    let payload_b = replay_b.split("\r\n\r\n").nth(1).unwrap();
    assert_eq!(
        payload_a, payload_b,
        "both subscribers replay the same history"
    );
    // Idle subscribers share one quiet ring: unfiltered reads settle
    // instead of merely surviving a fixed window that load can shift.
    let reads = wait_reads_quiet(|| feed.unfiltered_reads(), Duration::from_millis(300));
    feed.push(event(4, "live"));
    wake.publish(EventSeq::new(4));
    assert!(read_until(&mut first, "id: 4\n").contains("id: 4\n"));
    assert!(read_until(&mut second, "id: 4\n").contains("id: 4\n"));
    assert_eq!(
        feed.unfiltered_reads(),
        reads + 1,
        "one poller-driven fill serves every subscriber"
    );
    drop(first);
    drop(second);
    wait_released(&streams);
}
