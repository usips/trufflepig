use super::published_endpoint::{PublishedEndpoint, remove_published_endpoint};
use super::web_serve::{
    REFUSAL_DRAIN_LIMIT, RefusalDrains, refuse_queue_full, serve_connections,
    transient_accept_error, try_admit_drain,
};
use super::*;
use crate::board::board_protocol::ReadScope;
use crate::board::{
    board_ids::{BoardRef, EventSeq, RepoKey},
    board_protocol::{BOARD_API, BoardOp, BoardReply, BoardRequest, BoardResult, ClaimResume},
    board_vocabulary::{EntryText, PlanText, PlanTitle},
};
use event_stream::{EventStreams, SequencePoller};
use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::mpsc,
};
use web_guard::{BoardWebToken, WebGuard};
use web_ops::WebRequest;

struct AcceptHarness {
    _directory: tempfile::TempDir,
    _poller: SequencePoller,
    state: Arc<BoardWebState>,
}

fn accept_harness_with(reader: event_stream::FeedReader) -> AcceptHarness {
    let directory = crate::board::board_test_support::scratch("web-ring-stop-");
    let config = BoardConfig::for_database(directory.path().join("web.sqlite3"));
    let store = WebStore::open_at(
        BoardConfigCache::with_config(config),
        directory.path().join("runtime"),
    )
    .unwrap();
    let token = BoardWebToken::rotate_at(&directory.path().join("board-web.token")).unwrap();
    let guard = WebGuard::with_token("127.0.0.1:7341".parse().unwrap(), token).unwrap();
    // The poller stays alive: a dropped poller parks the filler before its
    // first fill, so only a live poller lets a poisoned feed stop the ring.
    let poller = SequencePoller::start(Arc::new(|| Ok(EventSeq::new(0)))).unwrap();
    let streams = EventStreams::new(reader, poller.handle());
    let expires = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let board_id = store
        .with_writer(&store.config(expires).unwrap(), expires, |writer| {
            writer.board_uuid()
        })
        .unwrap();
    AcceptHarness {
        _directory: directory,
        _poller: poller,
        state: Arc::new(BoardWebState {
            store: Arc::new(store),
            guard,
            streams,
            ingest: Default::default(),
            board_id,
        }),
    }
}

fn wait_ring_stopped(harness: &AcceptHarness) {
    let started = Instant::now();
    while harness.state.streams.reserve().is_ok() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "reserve() never observed the stopped filler"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn overview() -> BoardOp {
    BoardOp::Overview {
        scope: ReadScope::All,
        after: None,
        through: None,
        limit: 200,
    }
}

fn read(store: &WebStore, op: BoardOp) -> BoardReply {
    web_ops::execute(
        store,
        WebRequest {
            api: BOARD_API,
            op,
            project: None,
        },
        Instant::now() + Duration::from_secs(1),
    )
    .unwrap()
}

fn assert_stale_readers(
    pool: &ReaderPool,
    config: &BoardConfig,
    request: &BoardRequest,
    remaining: usize,
) {
    if remaining == 0 {
        return;
    }
    pool.with_reader(config, Instant::now() + Duration::from_secs(1), |reader| {
        let reply = reader.handle(request)?;
        let BoardResult::Plan(view) = reply.result else {
            panic!("expected plan");
        };
        assert_eq!(view.claims.len(), 1);
        assert!(view.claims.iter().all(|claim| claim.stale));
        assert_stale_readers(pool, config, request, remaining - 1);
        Ok(())
    })
    .unwrap();
}

fn stream_socket_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    (listener.accept().unwrap().0, client)
}

fn read_frame_until(client: &mut TcpStream, needle: &str) -> String {
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

fn subscribe(streams: &EventStreams) -> TcpStream {
    let (server, client) = stream_socket_pair();
    streams
        .spawn(
            server,
            event_stream::StreamRequest::parse(None, None, None).unwrap(),
            streams.reserve().unwrap(),
        )
        .unwrap();
    client
}

mod config_reload_tests;
mod project_request_tests;
mod serve_accept_tests;
mod serve_shutdown_tests;
mod stream_feed_tests;
mod web_store_tests;
