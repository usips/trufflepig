use super::sequence_poller::WakeResult;
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
    time::Instant,
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
        .set_read_timeout(Some(Duration::from_secs(5)))
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
                        event.plan == Some(plan) || relevant.contains(&(event.seq.get(), plan))
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
                started.elapsed() < Duration::from_secs(5),
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
        let count = crate::board::board_test_support::read_ignoring_interrupts(client, &mut buffer)
            .unwrap();
        assert!(count > 0, "stream closed before {needle}");
        bytes.extend_from_slice(&buffer[..count]);
    }
    String::from_utf8(bytes).unwrap()
}

fn wait_released(streams: &EventStreams) {
    let started = Instant::now();
    while streams.active() != 0 {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "stream permit retained"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

/// Polls `count` until it stays flat for `quiet`; a filler that keeps
/// reading on an idle subscriber never settles and fails the deadline.
fn wait_reads_quiet(count: impl Fn() -> usize, quiet: Duration) -> usize {
    let started = Instant::now();
    let mut settled = count();
    let mut settled_at = Instant::now();
    loop {
        let current = count();
        if current != settled {
            settled = current;
            settled_at = Instant::now();
        }
        if settled_at.elapsed() >= quiet {
            return settled;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "feed reads never settled: still growing at {settled}"
        );
        thread::sleep(Duration::from_millis(5));
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

mod live_ingest_tests;
mod outage_tests;
mod permit_handoff_tests;
mod plan_stream_tests;
mod poller_tests;
mod replay_permit_tests;
mod ring_cursor_tests;
mod socket_deadline_tests;
