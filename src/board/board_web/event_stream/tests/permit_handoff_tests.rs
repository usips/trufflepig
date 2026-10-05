use super::*;

#[test]
fn invalid_permit_handoff_releases_capacity_without_spawning() {
    let streams = streams(empty_reader());
    let other = super::streams(empty_reader());
    let (server, _client) = socket_pair();
    let server_addr = server.local_addr().unwrap();
    let request = StreamRequest::parse(None, None, None).unwrap();
    let refusal = streams
        .spawn(server, request, other.reserve().unwrap())
        .unwrap_err();
    assert_eq!(refusal.error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(refusal.socket.local_addr().unwrap(), server_addr);
    assert_eq!(other.active(), 0, "the refusal releases the slot");
}

#[test]
fn ahead_cursor_pokes_the_filler_and_rechecks_before_resyncing() {
    let feed = FakeFeed::seeded((1..=10).map(|seq| event(seq, "entry")).collect());
    let wake = SequenceWake::new();
    let streams = EventStreams::new(feed.reader(), wake.clone());
    feed.wait_total_reads(1);
    let mut client = spawn(&streams, 11);
    read_until(&mut client, "\r\n\r\n");
    // Fresh writes the poller has not published yet: without a poke the
    // filler never fills, and without a recheck the grace resyncs.
    feed.push(event(11, "caught up"));
    feed.push(event(12, "live"));
    let response = read_until(&mut client, "id: 12\n");
    assert!(
        !response.contains("event: resync\n"),
        "a subscribed wait must not resync when the data exists: {response}"
    );
    drop(client);
    wait_released(&streams);
}
