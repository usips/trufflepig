use super::route_render::proposal_diff;
use super::*;
use crate::board::board_backend::BoardBackend;
use crate::board::board_web::event_stream::{
    EventStreams, FeedReader, ReplayBatch, SequencePoller, StreamRequest,
};
use crate::board::board_web::web_guard::{BoardWebToken, WebGuard};
use crate::board::board_web::{WebStore, web_ops};
use crate::board::{
    board_config::{BoardConfig, BoardConfigCache},
    board_ids::{EventSeq, PlanId, PlanRevision},
    board_protocol::{BoardOp, BoardReply, BoardRequest, BoardResult},
    board_vocabulary::{EntryText, PlanText, PlanTitle},
};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener};
use std::sync::Arc;
use std::time::Duration;

struct RenderFixture {
    _directory: tempfile::TempDir,
    _poller: SequencePoller,
    state: Arc<BoardWebState>,
    config: BoardConfig,
    token: BoardWebToken,
    authority: String,
}

fn render_fixture() -> RenderFixture {
    // The filler always performs an initial unfiltered read; an empty feed
    // keeps the fixture ring alive without serving phantom events.
    render_fixture_with(Arc::new(|_, _, _| {
        Ok(ReplayBatch {
            latest: EventSeq::new(0),
            events: vec![],
        })
    }))
}

fn render_fixture_with(reader: FeedReader) -> RenderFixture {
    let directory = crate::board::board_test_support::scratch("web-render-");
    let config = BoardConfig::for_database(directory.path().join("web.sqlite3"));
    let store = WebStore::open_at(
        BoardConfigCache::with_config(config.clone()),
        directory.path().join("runtime"),
    )
    .unwrap();
    let token = BoardWebToken::rotate_at(&directory.path().join("board-web.token")).unwrap();
    let bind = TcpListener::bind("127.0.0.1:0").unwrap();
    let guard = WebGuard::with_token(bind.local_addr().unwrap(), token.clone()).unwrap();
    let authority = guard.authority().to_owned();
    let poller = SequencePoller::start(Arc::new(|| Ok(EventSeq::new(0)))).unwrap();
    let streams = EventStreams::new(reader, poller.handle());
    let board_id = store
        .with_writer(&config, Instant::now() + Duration::from_secs(5), |writer| {
            writer.board_uuid()
        })
        .unwrap();
    RenderFixture {
        _directory: directory,
        _poller: poller,
        state: Arc::new(BoardWebState {
            store: Arc::new(store),
            guard,
            streams,
            ingest: Default::default(),
            board_id,
        }),
        config,
        token,
        authority,
    }
}

fn execute(fixture: &RenderFixture, op: BoardOp) -> BoardReply {
    web_ops::execute(
        &fixture.state.store,
        WebRequest { api: BOARD_API, op },
        Instant::now() + Duration::from_secs(5),
    )
    .unwrap()
}

fn create_plan(fixture: &RenderFixture) -> PlanId {
    let created = execute(
        fixture,
        BoardOp::New {
            title: PlanTitle::new("Plan").unwrap(),
            body: PlanText::new("base\n").unwrap(),
            steward: None,
            repo_key: None,
        },
    );
    let BoardResult::Change(created) = created.result else {
        panic!("expected plan receipt")
    };
    created.plan.unwrap()
}

fn render_get(fixture: &RenderFixture, path: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    write!(
        client,
        "GET {path} HTTP/1.1\r\nHost: {}\r\nX-Board-Token: {}\r\n\r\n",
        fixture.authority,
        fixture.token.expose()
    )
    .unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    handle(server, Instant::now(), &fixture.state);
    let mut reply = String::new();
    client.read_to_string(&mut reply).unwrap();
    reply
}

fn authed_post(fixture: &RenderFixture, path: &str, body: &serde_json::Value) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    let body = body.to_string();
    write!(
        client,
        concat!(
            "POST {path} HTTP/1.1\r\n",
            "Host: {}\r\n",
            "Origin: http://{}\r\n",
            "X-Board-Token: {}\r\n",
            "Content-Type: application/json\r\n",
            "Content-Length: {}\r\n\r\n{body}"
        ),
        fixture.authority,
        fixture.authority,
        fixture.token.expose(),
        body.len(),
        path = path,
        body = body
    )
    .unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    handle(server, Instant::now(), &fixture.state);
    let mut reply = String::new();
    client.read_to_string(&mut reply).unwrap();
    reply
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

/// Reads one length-prefixed daemon request frame and returns its args.
fn read_daemon_args(stream: &mut std::os::unix::net::UnixStream) -> Vec<String> {
    use std::io::Read;
    let mut header = [0_u8; 4];
    stream.read_exact(&mut header).unwrap();
    let length = u32::from_be_bytes(header) as usize;
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).unwrap();
    let request: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    request["arguments"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap().to_owned())
        .collect()
}

/// Writes one length-prefixed success reply; a timed-out client may be
/// gone, and the reply still counts as served.
fn write_daemon_reply(stream: &mut std::os::unix::net::UnixStream, output: &str) {
    use std::io::Write;
    let reply = serde_json::json!({"status": "success", "output": output}).to_string();
    let _ = stream.write_all(&(reply.len() as u32).to_be_bytes());
    let _ = stream.write_all(reply.as_bytes());
}

fn challenge_post(fixture: &RenderFixture, body: &serde_json::Value) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    let body = body.to_string();
    write!(
        client,
        concat!(
            "POST /api/v1/challenge HTTP/1.1\r\n",
            "Host: {}\r\n",
            "Origin: http://{}\r\n",
            "Content-Type: application/json\r\n",
            "Content-Length: {}\r\n\r\n{body}"
        ),
        fixture.authority,
        fixture.authority,
        body.len(),
        body = body
    )
    .unwrap();
    handle(server, Instant::now(), &fixture.state);
    let mut reply = String::new();
    client.read_to_string(&mut reply).unwrap();
    reply
}

fn wrong_host_get(fixture: &RenderFixture, path: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    write!(
        client,
        "GET {path} HTTP/1.1\r\nHost: wrong.invalid\r\nX-Board-Token: {}\r\n\r\n",
        fixture.token.expose()
    )
    .unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    handle(server, Instant::now(), &fixture.state);
    let mut reply = String::new();
    client.read_to_string(&mut reply).unwrap();
    reply
}

mod asset_tests;
mod challenge_tests;
mod guard_tests;
mod ingest_tests;
mod method_tests;
mod render_tests;
mod reply_tests;
mod shell_tests;
mod stream_tests;
