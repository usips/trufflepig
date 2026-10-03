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

fn spawn(streams: &EventStreams, after: u64) -> TcpStream {
    let (server, client) = socket_pair();
    streams
        .spawn(
            server,
            StreamRequest::parse(None, Some(&after.to_string()), None).unwrap(),
            streams.reserve().unwrap(),
        )
        .unwrap();
    client
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
    let streams = streams(Arc::new(|_, _, _| unreachable!()));
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
    let reader_lease = Arc::new(Mutex::new(()));
    let lease = Arc::clone(&reader_lease);
    let streams = streams(Arc::new(move |after, _, limit| {
        let _lease = lease.lock().unwrap();
        assert_eq!(limit, 501);
        assert_eq!(after.get(), 4);
        Ok(ReplayBatch {
            latest: EventSeq::new(8),
            events: vec![event(5, "雪\n\"quoted\""), event(8, "next")],
        })
    }));
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
    assert!(
        reader_lease.try_lock().is_ok(),
        "reader held during network wait"
    );
    drop(client);
    wait_released(&streams);
}

#[test]
fn ahead_cursor_and_501_events_resync_close_without_advancing_id() {
    for (after, latest, count, reason) in [(20, 10, 0, "cursor_ahead"), (0, 501, 501, "replay_gap")]
    {
        let streams = streams(Arc::new(move |_, _, limit| {
            assert_eq!(limit, 501);
            Ok(ReplayBatch {
                latest: EventSeq::new(latest),
                events: (1..=count).map(|seq| event(seq, "entry")).collect(),
            })
        }));
        let mut client = spawn(&streams, after);
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert!(response.contains("event: resync\n"));
        assert!(response.contains(reason));
        assert!(!response.contains("\nid:"));
        assert!(!response.contains("event: board\n"));
        wait_released(&streams);
    }
}

#[test]
fn quiet_disconnect_errors_and_reader_panic_release_stream_slot() {
    let streams = streams(Arc::new(|_, _, _| {
        Ok(ReplayBatch {
            latest: EventSeq::new(0),
            events: vec![],
        })
    }));
    let mut client = spawn(&streams, 0);
    read_until(&mut client, "\r\n\r\n");
    drop(client);
    wait_released(&streams);
    for reader in [
        Arc::new(|_, _, _| {
            Err(BoardError::new(
                BoardErrorCode::BoardUnavailable,
                "unavailable",
            ))
        }) as FeedReader,
        Arc::new(|_, _, _| -> Result<ReplayBatch, BoardError> { panic!("reader panic") })
            as FeedReader,
    ] {
        let streams = super::tests::streams(reader);
        let mut client = spawn(&streams, 0);
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        wait_released(&streams);
    }
}

#[test]
fn wake_during_materialization_cannot_be_lost_before_wait() {
    let wake = SequenceWake::new();
    let publish = wake.clone();
    let reads = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&reads);
    let streams = EventStreams::new(
        Arc::new(move |after, _, _| {
            if count.fetch_add(1, Ordering::AcqRel) == 0 {
                publish.publish(EventSeq::new(1));
                return Ok(ReplayBatch {
                    latest: EventSeq::new(0),
                    events: vec![],
                });
            }
            assert_eq!(after.get(), 0);
            Ok(ReplayBatch {
                latest: EventSeq::new(1),
                events: vec![event(1, "committed during read")],
            })
        }),
        wake,
    );
    let mut client = spawn(&streams, 0);
    assert!(read_until(&mut client, "committed during read").contains("id: 1\n"));
    assert_eq!(reads.load(Ordering::Acquire), 2);
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
    let streams = streams(Arc::new(|_, _, _| unreachable!()));
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
    let streams = streams(Arc::new(|_, _, _| unreachable!()));
    let other = super::tests::streams(Arc::new(|_, _, _| unreachable!()));
    let (server, _client) = socket_pair();
    let request = StreamRequest::parse(None, None, None).unwrap();
    assert_eq!(
        streams
            .spawn(server, request, other.reserve().unwrap())
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(other.active(), 0);
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
        let streams = EventStreams::new(Arc::new(|_, _, _| unreachable!()), wake.clone());
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

#[test]
fn polling_outage_closes_live_stream_and_restores_capacity_and_ordered_replay() {
    let wake = SequenceWake::new();
    wake.publish(EventSeq::new(2));
    let streams = EventStreams::new(
        Arc::new(|after, _, _| {
            Ok(ReplayBatch {
                latest: EventSeq::new(2),
                events: (1..=2)
                    .filter(|seq| *seq > after.get())
                    .map(|seq| event(seq, "entry"))
                    .collect(),
            })
        }),
        wake.clone(),
    );
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
    let streams = EventStreams::new(Arc::new(|_, _, _| unreachable!()), wake.clone());
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
fn plan_filtered_stream_replays_scoped_aggregate_commit_and_advances_cursor() {
    let latest = Arc::new(AtomicU64::new(2));
    let snapshot = Arc::clone(&latest);
    let reads = Arc::new(Mutex::new(Vec::new()));
    let read_cursors = Arc::clone(&reads);
    let wake = SequenceWake::new();
    wake.publish(EventSeq::new(2));
    let streams = EventStreams::new(
        Arc::new(move |after, plan, limit| {
            assert_eq!(plan, Some(PlanId::new(7).unwrap()));
            assert_eq!(limit, 501);
            read_cursors.lock().unwrap().push(after.get());
            let latest = snapshot.load(Ordering::Acquire);
            let events = (1..=latest)
                .filter(|seq| *seq > after.get())
                .map(|seq| {
                    if seq == 2 {
                        let mut aggregate = event(seq, "mixed-plan commit");
                        aggregate.plan = None;
                        aggregate.kind = EntryKind::Commit;
                        aggregate
                    } else {
                        event(seq, "scoped progress")
                    }
                })
                .collect();
            Ok(ReplayBatch {
                latest: EventSeq::new(latest),
                events,
            })
        }),
        wake.clone(),
    );
    let (server, mut client) = socket_pair();
    let request = StreamRequest::parse(None, Some("1"), Some("P7")).unwrap();
    streams
        .spawn(server, request, streams.reserve().unwrap())
        .unwrap();
    let replay = read_until(&mut client, "}\n\n");
    let data = replay
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .unwrap();
    let aggregate: EventRecord = serde_json::from_str(data).unwrap();
    assert!(replay.contains("id: 2\n"));
    assert_eq!(aggregate.kind, EntryKind::Commit);
    assert_eq!(aggregate.plan, None);
    latest.store(3, Ordering::Release);
    wake.publish(EventSeq::new(3));
    assert!(read_until(&mut client, "}\n\n").contains("id: 3\n"));
    assert_eq!(*reads.lock().unwrap(), vec![1, 2]);
    drop(client);
    wait_released(&streams);
}
