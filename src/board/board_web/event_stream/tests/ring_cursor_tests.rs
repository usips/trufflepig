use super::*;

#[test]
fn backlog_of_500_replays_but_501_resyncs_with_replay_gap() {
    for (count, resync) in [(500, false), (501, true)] {
        let feed = FakeFeed::seeded((1..=count).map(|seq| event(seq, "entry")).collect());
        let streams = streams(feed.reader());
        let mut client = spawn(&streams, 0);
        if resync {
            let mut response = String::new();
            client.read_to_string(&mut response).unwrap();
            assert!(response.contains("event: resync\n"), "{count}: {response}");
            assert!(response.contains("replay_gap"), "{count}: {response}");
            assert!(!response.contains("event: board\n"), "{count}: {response}");
        } else {
            let response = read_until(&mut client, &format!("id: {count}\n"));
            assert!(response.contains("id: 1\n"), "{count}: {response}");
            assert!(!response.contains("event: resync\n"), "{count}: {response}");
            drop(client);
        }
        wait_released(&streams);
    }
}

#[test]
fn ring_window_serves_cursors_inside_and_resyncs_before_oldest() {
    let feed = FakeFeed::seeded((1..=600).map(|seq| event(seq, "entry")).collect());
    let streams = streams(feed.reader());
    let mut replay = spawn(&streams, 200);
    let response = read_until(&mut replay, "id: 600\n");
    assert!(response.contains("id: 201\n"), "{response}");
    assert!(!response.contains("id: 105\n"), "{response}");
    assert!(!response.contains("event: resync\n"), "{response}");
    drop(replay);
    let mut lagging = spawn(&streams, 50);
    let mut refused = String::new();
    lagging.read_to_string(&mut refused).unwrap();
    assert!(refused.contains("event: resync\n"), "{refused}");
    assert!(refused.contains("replay_gap"), "{refused}");
    wait_released(&streams);
}

#[test]
fn cursor_ahead_of_the_database_resyncs_after_a_bounded_grace() {
    let feed = FakeFeed::seeded((1..=10).map(|seq| event(seq, "entry")).collect());
    let streams = streams(feed.reader());
    let started = Instant::now();
    let mut client = spawn(&streams, 20);
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    assert!(response.contains("event: resync\n"), "{response}");
    assert!(response.contains("cursor_ahead"), "{response}");
    assert!(!response.contains("\nid:"), "{response}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the ahead grace is bounded"
    );
    wait_released(&streams);
}

#[test]
fn cursor_ahead_during_poller_lag_waits_for_the_fill_instead_of_resyncing() {
    let feed = FakeFeed::seeded((1..=10).map(|seq| event(seq, "entry")).collect());
    let wake = SequenceWake::new();
    let streams = EventStreams::new(feed.reader(), wake.clone());
    feed.wait_total_reads(1);
    let mut client = spawn(&streams, 11);
    read_until(&mut client, "\r\n\r\n");
    feed.push(event(11, "caught up"));
    wake.publish(EventSeq::new(11));
    feed.push(event(12, "live"));
    wake.publish(EventSeq::new(12));
    let response = read_until(&mut client, "id: 12\n");
    assert!(
        !response.contains("event: resync\n"),
        "a lagging poller must not force a resync: {response}"
    );
    drop(client);
    wait_released(&streams);
}

#[test]
fn restored_database_reseeds_the_ring_and_resyncs_ahead_cursors() {
    let feed = FakeFeed::seeded((1..=10).map(|seq| event(seq, "original")).collect());
    let wake = SequenceWake::new();
    let streams = EventStreams::new(feed.reader(), wake.clone());
    let mut steady = spawn(&streams, 10);
    read_until(&mut steady, "\r\n\r\n");
    {
        let mut events = feed.events.lock().unwrap();
        events.clear();
        for seq in 1..=5 {
            events.push(event(seq, "restored"));
        }
    }
    wake.publish(EventSeq::new(5));
    let mut response = String::new();
    steady.read_to_string(&mut response).unwrap();
    assert!(response.contains("cursor_ahead"), "{response}");
    let mut replay = spawn(&streams, 3);
    let frames = read_until(&mut replay, "id: 5\n");
    assert!(frames.contains("restored"), "{frames}");
    assert!(!frames.contains("original"), "{frames}");
    drop(replay);
    wait_released(&streams);
}
