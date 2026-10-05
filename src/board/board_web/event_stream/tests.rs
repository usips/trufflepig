use super::*;
use crate::board::{
    board_actor::{BoardActor, HarnessLabel},
    board_ids::{BoardRef, EntryId},
    board_vocabulary::{EntryKind, EntryText},
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64},
    },
};

fn event(seq: u64, summary: &str) -> EventRecord {
    EventRecord {
        via: None,
        seq: EventSeq::new(seq),
        plan: Some(PlanId::new(7).unwrap()),
        kind: EntryKind::Progress,
        subject: BoardRef::Entry(EntryId::new(seq).unwrap()),
        to: None,
        actor: BoardActor::new("josh", "host", HarnessLabel::parse("codex").unwrap(), "s1")
            .unwrap(),
        model: None,
        effort: None,
        summary: EntryText::new(summary).unwrap(),
        created_at: 1,
    }
}

fn socket_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    (listener.accept().unwrap().0, client)
}

fn streams(reader: FeedReader) -> EventStreams {
    EventStreams::new(reader, SequenceWake::new())
}

fn empty_reader() -> FeedReader {
    Arc::new(|_, _, _| {
        Ok(ReplayBatch {
            latest: EventSeq::new(0),
            events: vec![],
        })
    })
}

/// A faithful `read_event_batch` double: ascending committed events, plan
/// relevance by record or override, and `require_plan` for known plans.
#[derive(Default)]
struct FakeFeed {
    events: Mutex<Vec<EventRecord>>,
    relevant: Mutex<Vec<(u64, PlanId)>>,
    known_plans: Mutex<Vec<PlanId>>,
    fail_plans: Mutex<Vec<PlanId>>,
    reads: Mutex<Vec<(u64, Option<PlanId>)>>,
    down: AtomicBool,
}

impl FakeFeed {
    fn seeded(events: Vec<EventRecord>) -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(events),
            ..Self::default()
        })
    }

    fn reader(self: &Arc<Self>) -> FeedReader {
        let feed = Arc::clone(self);
        Arc::new(move |after, plan, limit| {
            feed.reads.lock().unwrap().push((after.get(), plan));
            if feed.down.load(Ordering::Acquire) {
                return Err(BoardError::new(
                    BoardErrorCode::BoardUnavailable,
                    "feed unavailable",
                ));
            }
            let events = feed.events.lock().unwrap();
            let latest = events.last().map(|event| event.seq).unwrap_or_default();
            if let Some(plan) = plan {
                if feed.fail_plans.lock().unwrap().contains(&plan) {
                    return Err(BoardError::new(
                        BoardErrorCode::BoardUnavailable,
                        format!("plan read failed {plan}"),
                    ));
                }
                let known = feed.known_plans.lock().unwrap();
                if !known.is_empty() && !known.contains(&plan) {
                    return Err(BoardError::new(
                        BoardErrorCode::InvalidReference,
                        format!("unknown plan {plan}"),
                    ));
                }
            }
            let relevant = feed.relevant.lock().unwrap();
            let batch = events
                .iter()
                .filter(|event| event.seq > after)
                .filter(|event| {
                    plan.map_or(true, |plan| {
                        event.plan == Some(plan)
                            || relevant.contains(&(event.seq.get(), plan))
                    })
                })
                .take(limit)
                .cloned()
                .collect();
            Ok(ReplayBatch {
                latest,
                events: batch,
            })
        })
    }

    fn push(&self, event: EventRecord) {
        self.events.lock().unwrap().push(event);
    }

    fn read_count(&self) -> usize {
        self.reads.lock().unwrap().len()
    }

    fn unfiltered_reads(&self) -> usize {
        self.reads
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, plan)| plan.is_none())
            .count()
    }

    fn wait_total_reads(&self, at_least: usize) {
        let started = Instant::now();
        while self.read_count() < at_least {
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "feed reads stalled at {}",
                self.read_count()
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}

fn read_until(client: &mut TcpStream, needle: &str) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    while !String::from_utf8_lossy(&bytes).contains(needle) {
        let count = client.read(&mut buffer).unwrap();
        assert!(count > 0, "stream closed before {needle}");
        bytes.extend_from_slice(&buffer[..count]);
    }
    String::from_utf8(bytes).unwrap()
}

fn wait_released(streams: &EventStreams) {
    let started = Instant::now();
    while streams.active() != 0 {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "stream permit retained"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn spawn_request(streams: &EventStreams, request: StreamRequest) -> TcpStream {
    let (server, client) = socket_pair();
    streams
        .spawn(server, request, streams.reserve().unwrap())
        .unwrap();
    client
}

fn spawn(streams: &EventStreams, after: u64) -> TcpStream {
    spawn_request(
        streams,
        StreamRequest::parse(None, Some(&after.to_string()), None).unwrap(),
    )
}

#[test]
fn live_stream_response_carries_security_headers() {
    let streams = streams(empty_reader());
    let mut client = spawn(&streams, 0);
    let headers = read_until(&mut client, "\r\n\r\n");
    assert!(
        headers.contains(concat!(
            "Content-Security-Policy: default-src 'self'; frame-ancestors 'none'; ",
            "base-uri 'none'; object-src 'none'; form-action 'none'\r\n"
        )),
        "{headers}"
    );
    assert!(headers.contains("Referrer-Policy: no-referrer\r\n"), "{headers}");
    assert!(
        headers.contains("X-Content-Type-Options: nosniff\r\n"),
        "{headers}"
    );
    assert!(headers.contains("Cache-Control: no-store\r\n"), "{headers}");
    drop(client);
    wait_released(&streams);
}

#[test]
fn ingest_results_reach_live_subscribers_without_an_event_cursor() {
    let streams = streams(empty_reader());
    let mut client = spawn(&streams, 0);
    read_until(&mut client, "\r\n\r\n");
    streams.publish_ingest(&serde_json::json!({"inserted": 3}));
    let frame = read_until(&mut client, "event: ingest\n");
    assert!(frame.contains("data: {\"inserted\":3}\n\n"), "{frame}");
    assert!(!frame.contains("\nid:"), "{frame}");
    drop(client);
    wait_released(&streams);
}

#[test]
fn ingest_results_before_subscription_replay_for_ticket_filtering() {
    let streams = streams(empty_reader());
    streams.publish_ingest(&serde_json::json!({"inserted": 1}));
    let mut client = spawn(&streams, 0);
    let replay = read_until(&mut client, "\"inserted\":1");
    assert!(replay.contains("event: ingest\n"), "{replay}");
    drop(client);
    wait_released(&streams);
}

#[test]
fn resubscribed_stream_receives_receipts_published_while_away() {
    let streams = streams(empty_reader());
    let mut client = spawn(&streams, 0);
    read_until(&mut client, "\r\n\r\n");
    drop(client);
    wait_released(&streams);
    streams.publish_ingest(&serde_json::json!({"inserted": 7}));
    let mut resubscribed = spawn(&streams, 0);
    let replay = read_until(&mut resubscribed, "\"inserted\":7");
    assert!(replay.contains("event: ingest\n"), "{replay}");
    drop(resubscribed);
    wait_released(&streams);
}

#[test]
fn resubscribe_replays_only_the_last_eight_receipts() {
    let streams = streams(empty_reader());
    for n in 1..=10 {
        streams.publish_ingest(&serde_json::json!({"n": n}));
    }
    let mut client = spawn(&streams, 0);
    read_until(&mut client, "\r\n\r\n");
    let replay = read_until(&mut client, "\"n\":10");
    for n in 3..=10 {
        assert!(replay.contains(&format!("\"n\":{n}")), "{replay}");
    }
    assert!(!replay.contains("\"n\":1}"), "{replay}");
    assert!(!replay.contains("\"n\":2}"), "{replay}");
    drop(client);
    wait_released(&streams);
}

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
    let reads = feed.read_count();
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        feed.read_count(),
        reads,
        "an idle subscriber must not drive further reads"
    );
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
    assert_eq!(payload_a, payload_b, "both subscribers replay the same history");
    let reads = feed.unfiltered_reads();
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        feed.unfiltered_reads(),
        reads,
        "idle subscribers share one quiet ring"
    );
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
    let panicking = Arc::new(|_, _, _| -> Result<ReplayBatch, BoardError> {
        panic!("feed panic")
    }) as FeedReader;
    let panicked = streams(panicking);
    // A poisoned feed stops the ring, so reserve() refuses new streams
    // instead of spawning closes; no slot is held or leaked.
    let started = Instant::now();
    while panicked.reserve().is_ok() {
        assert!(
            started.elapsed() < Duration::from_secs(2),
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
fn outage_between_reservation_and_spawn_closes_before_http_success() {
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
    assert!(response.is_empty());
    wait_released(&streams);
}

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
fn sequence_wakes_coalesce_and_poller_retries_at_bounded_cadence() {
    let wake = SequenceWake::new();
    let observed = wake.generation();
    for seq in 1..=100 {
        wake.publish(EventSeq::new(seq));
    }
    assert_eq!(
        wake.wait(observed, Duration::from_secs(1)),
        WakeResult::Changed
    );
    assert_eq!(
        wake.wait(wake.generation(), Duration::from_millis(1)),
        WakeResult::Timeout
    );
    let calls = Arc::new(AtomicU64::new(0));
    let called = Arc::clone(&calls);
    let poller = SequencePoller::start(Arc::new(move || {
        called.fetch_add(1, Ordering::AcqRel);
        Err(BoardError::new(BoardErrorCode::DatabaseLocked, "busy"))
    }))
    .unwrap();
    thread::sleep(Duration::from_millis(550));
    let handle = poller.handle();
    drop(poller);
    assert!((2..=4).contains(&calls.load(Ordering::Acquire)));
    assert_eq!(
        handle.wait(handle.generation(), Duration::from_secs(1)),
        WakeResult::Stopped
    );
}

#[test]
fn event_snapshot_rejects_unordered_or_cursor_overlapping_records() {
    let batch = ReplayBatch {
        latest: EventSeq::new(9),
        events: vec![event(8, "a"), event(7, "b")],
    };
    assert!(batch.validate(EventSeq::new(0)).is_err());
    let batch = ReplayBatch {
        latest: EventSeq::new(9),
        events: vec![event(8, "a")],
    };
    assert!(batch.validate(EventSeq::new(8)).is_err());
    let batch = ReplayBatch {
        latest: EventSeq::new(9),
        events: vec![event(10, "b")],
    };
    assert!(batch.validate(EventSeq::new(0)).is_err());
}

#[test]
fn polling_busy_or_panic_rejects_admission_then_recovers_unchanged_sequence() {
    for panic_first in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let called = Arc::clone(&calls);
        let allow_success = Arc::new(AtomicBool::new(false));
        let success_gate = Arc::clone(&allow_success);
        let poller = SequencePoller::start(Arc::new(move || {
            let call = called.fetch_add(1, Ordering::AcqRel);
            if !success_gate.load(Ordering::Acquire) {
                if panic_first && call == 0 {
                    panic!("sequence reader panic");
                }
                return Err(BoardError::new(BoardErrorCode::DatabaseLocked, "busy"));
            }
            Ok(EventSeq::new(0))
        }))
        .unwrap();
        let wake = poller.handle();
        assert_eq!(
            wake.wait(wake.generation(), Duration::from_secs(1)),
            WakeResult::Unavailable
        );
        let unavailable_generation = wake.generation();
        let streams = EventStreams::new(empty_reader(), wake.clone());
        assert_eq!(
            streams.reserve().err().unwrap().code,
            BoardErrorCode::BoardUnavailable
        );
        allow_success.store(true, Ordering::Release);
        let started = Instant::now();
        while !wake.available() {
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "poller failed to recover"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            wake.wait(unavailable_generation, Duration::ZERO),
            WakeResult::Changed
        );
        assert!(calls.load(Ordering::Acquire) >= 2);
        drop(streams.reserve().unwrap());
        assert_eq!(streams.active(), 0);
        drop(poller);
        assert_eq!(
            wake.wait(wake.generation(), Duration::ZERO),
            WakeResult::Stopped
        );
        assert_eq!(
            streams.reserve().err().unwrap().code,
            BoardErrorCode::BoardUnavailable
        );
    }
}

#[cfg(unix)]
#[test]
fn slow_tcp_subscriber_hits_absolute_send_deadline() {
    use std::os::fd::AsRawFd;
    let (mut server, _client) = socket_pair();
    let size: libc::c_int = 4096;
    // The socket belongs to this test; lowering its send buffer forces backpressure.
    assert_eq!(
        unsafe {
            libc::setsockopt(
                server.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&size as *const libc::c_int).cast(),
                std::mem::size_of_val(&size) as libc::socklen_t,
            )
        },
        0
    );
    let bytes = vec![b'x'; 1024 * 1024];
    let started = Instant::now();
    let error = stream_socket::send(&mut server, &bytes, Duration::from_millis(80)).unwrap_err();
    assert!(matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn partial_writes_share_one_deadline_and_drop_their_permit_on_error() {
    struct PartialWriter {
        timeouts: Mutex<Vec<Duration>>,
    }
    impl Write for PartialWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            thread::sleep(Duration::from_millis(5));
            Ok(1)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl stream_socket::DeadlineWriter for PartialWriter {
        fn remaining_timeout(&self, timeout: Duration) -> io::Result<()> {
            self.timeouts.lock().unwrap().push(timeout);
            Ok(())
        }
    }
    let streams = streams(empty_reader());
    let permit = streams.reserve().unwrap();
    let mut writer = PartialWriter {
        timeouts: Mutex::new(Vec::new()),
    };
    assert_eq!(
        stream_socket::send(&mut writer, &[1; 100], Duration::from_millis(25))
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    let timeouts = writer.timeouts.lock().unwrap();
    assert!(timeouts.windows(2).all(|pair| pair[0] > pair[1]));
    drop(permit);
    assert_eq!(streams.active(), 0);
}

#[test]
fn invalid_permit_handoff_releases_capacity_without_spawning() {
    let streams = streams(empty_reader());
    let other = super::tests::streams(empty_reader());
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
