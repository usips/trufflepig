use super::*;

#[test]
fn plan_filtered_stream_replays_annotated_relevance_and_advances_cursor() {
    let mut aggregate = event(2, "mixed-plan commit");
    aggregate.plan = None;
    aggregate.kind = EntryKind::Commit;
    let mut other = event(3, "other plan progress");
    other.plan = Some(PlanId::new(8).unwrap());
    let feed = FakeFeed::seeded(vec![event(1, "scoped progress"), aggregate, other]);
    feed.relevant
        .lock()
        .unwrap()
        .push((2, PlanId::new(7).unwrap()));
    *feed.known_plans.lock().unwrap() = vec![PlanId::new(7).unwrap(), PlanId::new(8).unwrap()];
    let wake = SequenceWake::new();
    let streams = EventStreams::new(feed.reader(), wake.clone());
    let mut client = spawn_request(
        &streams,
        StreamRequest::parse(None, Some("0"), Some("P7")).unwrap(),
    );
    let replay = read_until(&mut client, "id: 2\n");
    assert!(replay.contains("id: 1\n"), "{replay}");
    assert!(!replay.contains("id: 3\n"), "{replay}");
    let data = replay
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .last()
        .unwrap();
    let delivered: EventRecord = serde_json::from_str(data).unwrap();
    assert_eq!(delivered.kind, EntryKind::Commit);
    assert_eq!(delivered.plan, None);
    feed.push(event(4, "more scoped progress"));
    wake.publish(EventSeq::new(4));
    assert!(read_until(&mut client, "id: 4\n").contains("id: 4\n"));
    {
        let reads = feed.reads.lock().unwrap();
        assert!(
            reads.contains(&(0, Some(PlanId::new(7).unwrap()))),
            "relevance reads carry the plan filter: {reads:?}"
        );
    }
    drop(client);
    wait_released(&streams);
}

#[test]
fn unknown_plan_filter_is_rejected_before_success() {
    let feed = FakeFeed::seeded(vec![event(1, "scoped progress")]);
    *feed.known_plans.lock().unwrap() = vec![PlanId::new(7).unwrap()];
    let streams = streams(feed.reader());
    let mut client = spawn_request(
        &streams,
        StreamRequest::parse(None, Some("0"), Some("P9")).unwrap(),
    );
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 404 "), "{response}");
    assert!(
        response.contains("\"code\":\"invalid_reference\""),
        "{response}"
    );
    assert!(!response.contains("HTTP/1.1 200"), "{response}");
    assert!(!response.contains("event: board\n"), "{response}");
    wait_released(&streams);
}

#[test]
fn fill_during_subscription_reaches_the_waiting_subscriber() {
    let feed = FakeFeed::seeded(vec![]);
    let wake = SequenceWake::new();
    let streams = EventStreams::new(feed.reader(), wake.clone());
    let mut client = spawn(&streams, 0);
    read_until(&mut client, "\r\n\r\n");
    feed.push(event(1, "committed during subscription"));
    wake.publish(EventSeq::new(1));
    assert!(read_until(&mut client, "id: 1\n").contains("committed during subscription"));
    drop(client);
    wait_released(&streams);
}

#[test]
fn one_plan_read_error_fails_only_that_plan_stream() {
    let feed = FakeFeed::seeded(vec![event(1, "scoped progress")]);
    *feed.known_plans.lock().unwrap() = vec![PlanId::new(7).unwrap(), PlanId::new(8).unwrap()];
    feed.fail_plans
        .lock()
        .unwrap()
        .push(PlanId::new(8).unwrap());
    let wake = SequenceWake::new();
    let streams = EventStreams::new(feed.reader(), wake.clone());
    // Reconnecting clients keep a failing plan registered; hold its lease
    // for the whole test so the filler must contain the error per plan.
    let _failing_lease = streams.ring.subscribe_plan(PlanId::new(8).unwrap());
    let mut healthy = spawn_request(
        &streams,
        StreamRequest::parse(None, Some("0"), Some("P7")).unwrap(),
    );
    let replay = read_until(&mut healthy, "id: 1\n");
    assert!(replay.contains("scoped progress"), "{replay}");
    let mut failing = spawn_request(
        &streams,
        StreamRequest::parse(None, Some("0"), Some("P8")).unwrap(),
    );
    let mut closed = String::new();
    failing.read_to_string(&mut closed).unwrap();
    assert!(closed.starts_with("HTTP/1.1 200 "), "{closed}");
    assert!(!closed.contains("event: board\n"), "{closed}");
    feed.push(event(2, "still served"));
    wake.publish(EventSeq::new(2));
    let live = read_until(&mut healthy, "id: 2\n");
    assert!(!live.contains("event: resync\n"), "{live}");
    drop(healthy);
    wait_released(&streams);
}
