use super::*;
use crate::board::board_backend::BoardBackend;
use crate::board::board_web::event_stream::{
    EventStreams, FeedReader, ReplayBatch, SequencePoller, StreamRequest,
};
use crate::board::board_web::web_guard::{BoardWebToken, WebGuard};
use crate::board::board_web::{WebStore, web_ops};
use crate::board::{
    board_config::{BoardConfig, BoardConfigCache},
    board_ids::{EventSeq, PlanId},
    board_protocol::{BoardReply, BoardRequest, BoardResult},
    board_vocabulary::{EntryText, PlanText, PlanTitle},
};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener};
use std::sync::Arc;
use std::time::Duration;

#[test]
fn unavailable_board_errors_include_retry_after_and_security_headers() {
    for code in [
        BoardErrorCode::DaemonBusy,
        BoardErrorCode::BoardUnavailable,
        BoardErrorCode::DatabaseLocked,
    ] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        send_board_error(&mut server, BoardError::new(code, "unavailable"));
        server.shutdown(std::net::Shutdown::Write).unwrap();
        let mut reply = String::new();
        client.read_to_string(&mut reply).unwrap();
        assert!(reply.starts_with("HTTP/1.1 503 "));
        assert!(reply.contains("Retry-After: 1\r\n"));
        assert!(reply.contains("Cache-Control: no-store\r\n"));
        assert!(reply.contains("X-Content-Type-Options: nosniff\r\n"));
        assert!(reply.contains("Content-Security-Policy:"));
    }
}

#[test]
fn proposal_diff_preserves_all_lines_and_the_earlier_snapshot() {
    let directory = crate::board::board_test_support::scratch("web-proposal-diff-");
    let config = BoardConfig::for_database(directory.path().join("web.sqlite3"));
    let store = WebStore::open_at(
        BoardConfigCache::with_config(config.clone()),
        directory.path().join("runtime"),
    )
    .unwrap();
    let expires = Instant::now() + Duration::from_secs(5);
    let execute = |op| web_ops::execute(&store, WebRequest { api: BOARD_API, op }, expires);
    let created = execute(BoardOp::New {
        title: PlanTitle::new("Plan").unwrap(),
        body: PlanText::new("base\n").unwrap(),
        steward: None,
        repo_key: None,
    })
    .unwrap();
    let BoardResult::Change(created) = created.result else {
        panic!("expected plan receipt")
    };
    let plan = created.plan.unwrap();
    let body = (0..1001)
        .map(|line| format!("row {line}\n"))
        .collect::<String>();
    let proposal = store
        .with_writer(&config, expires, |writer| {
            writer.handle(&BoardRequest::new(
                config.actor(Some("codex"), Some("worker")).unwrap(),
                BoardOp::Propose {
                    base: PlanRevision::new(plan, 1).unwrap(),
                    body: PlanText::new(body.clone()).unwrap(),
                    summary: EntryText::new("complete proposal").unwrap(),
                    supersedes: None,
                },
            ))
        })
        .unwrap();
    let BoardResult::Change(proposal) = proposal.result else {
        panic!("expected proposal receipt")
    };
    let mut earlier = None;
    let mut later = None;
    let diff = proposal_diff(&proposal.entry.to_string(), |target| {
        let reply = execute(BoardOp::Show { target })?;
        if earlier.is_none() {
            earlier = reply.snapshot_seq;
            execute(BoardOp::New {
                title: PlanTitle::new("Intervening event").unwrap(),
                body: PlanText::new("").unwrap(),
                steward: None,
                repo_key: None,
            })?;
        } else {
            later = reply.snapshot_seq;
        }
        Ok(reply)
    })
    .unwrap();
    assert!(later.unwrap() > earlier.unwrap());
    assert_eq!(diff["snapshot_seq"], serde_json::json!(earlier));
    let added = diff["hunks"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|hunk| hunk["added"].as_array().unwrap())
        .map(|line| line.as_str().unwrap())
        .collect::<String>();
    assert_eq!(added, body);
    assert!(added.ends_with("row 1000\n"));
}

struct RenderFixture {
    _directory: tempfile::TempDir,
    _poller: SequencePoller,
    state: Arc<WebState>,
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
    RenderFixture {
        _directory: directory,
        _poller: poller,
        state: Arc::new(WebState {
            store: Arc::new(store),
            guard,
            streams,
            ingest: Default::default(),
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

#[test]
fn shell_and_static_assets_defeat_caching() {
    let fixture = render_fixture();
    for path in ["/", "/board_web_main.js", "/board_web.css", "/board_stream.js"] {
        let reply = render_get(&fixture, path);
        assert!(reply.starts_with("HTTP/1.1 200 "), "{path}: {reply}");
        assert!(
            reply.contains("Cache-Control: no-store\r\n"),
            "{path}: {reply}"
        );
    }
}

#[test]
fn method_refusals_list_the_permitted_methods() {
    let fixture = render_fixture();
    let reply = authed_post(&fixture, "/", &serde_json::json!({}));
    assert!(reply.starts_with("HTTP/1.1 405 "), "{reply}");
    assert!(reply.contains("Allow: GET\r\n"), "{reply}");
    let reply = render_get(&fixture, "/api/v1/challenge");
    assert!(reply.starts_with("HTTP/1.1 405 "), "{reply}");
    assert!(reply.contains("Allow: POST\r\n"), "{reply}");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    write!(client, "HEAD / HTTP/1.1\r\nHost: {}\r\n\r\n", fixture.authority).unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    handle(server, Instant::now(), &fixture.state);
    let mut reply = String::new();
    client.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 405 "), "{reply}");
    assert!(reply.contains("Allow: GET, POST\r\n"), "{reply}");
}

#[test]
fn early_errors_send_their_reply_before_closing_on_unread_input() {
    let fixture = render_fixture();
    let mut oversized_headers = b"GET / HTTP/1.1\r\nHost: x\r\nX-Pad: ".to_vec();
    oversized_headers.resize(64 * 1024, b'a');
    let mut oversized_body =
        b"POST /api/v1/board HTTP/1.1\r\nHost: x\r\nContent-Length: 999999\r\n\r\n".to_vec();
    oversized_body.resize(oversized_body.len() + 4096, b'b');
    for (bytes, status) in [(oversized_headers, "431"), (oversized_body, "413")] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        client.write_all(&bytes).unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        handle(server, Instant::now(), &fixture.state);
        let mut reply = Vec::new();
        client.read_to_end(&mut reply).unwrap();
        let reply = String::from_utf8(reply).unwrap();
        assert!(reply.starts_with(&format!("HTTP/1.1 {status} ")), "{reply}");
        assert!(reply.ends_with("}}"), "{reply}");
    }
}

#[test]
fn event_stream_rejects_a_bad_plan_filter_before_success() {
    let fixture = render_fixture();
    for query in ["plan=bogus", "plan=P0", "plan=7", "plan="] {
        let reply = render_get(&fixture, &format!("/api/v1/events?{query}"));
        assert!(reply.starts_with("HTTP/1.1 400 "), "{query}: {reply}");
        assert!(
            reply.contains("\"code\":\"invalid_options\""),
            "{query}: {reply}"
        );
    }
}

#[test]
fn event_stream_capacity_refusal_answers_503_with_retry_after() {
    let fixture = render_fixture();
    let permits: Vec<_> = (0..crate::board::board_web::event_stream::STREAM_LIMIT)
        .map(|_| fixture.state.streams.reserve().unwrap())
        .collect();
    let reply = render_get(&fixture, "/api/v1/events");
    assert!(reply.starts_with("HTTP/1.1 503 "), "{reply}");
    assert!(reply.contains("Retry-After: 1\r\n"), "{reply}");
    drop(permits);
}

#[test]
fn refused_event_stream_spawn_answers_503_instead_of_silence() {
    let fixture = render_fixture();
    let poller = SequencePoller::start(Arc::new(|| Ok(EventSeq::new(0)))).unwrap();
    let foreign = EventStreams::new(
        Arc::new(|_, _, _| unreachable!("no feed reads")),
        poller.handle(),
    );
    let permit = foreign.reserve().unwrap();
    let request = StreamRequest::parse(None, None, None).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    stream_subscription(server, Ok((request, permit)), &fixture.state.streams);
    let mut reply = String::new();
    client.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 503 "), "{reply}");
    assert!(reply.contains("Retry-After: 1\r\n"), "{reply}");
    assert_eq!(foreign.active(), 0, "refused spawn released the slot");
}

#[test]
fn ingest_posts_answer_202_without_waiting_for_the_relay() {
    let fixture = render_fixture();
    for _ in 0..2 {
        let reply = authed_post(&fixture, "/api/v1/ingest", &serde_json::json!({"api": BOARD_API}));
        assert!(reply.starts_with("HTTP/1.1 202 "), "{reply}");
        assert!(reply.contains("\"queued\""), "{reply}");
    }
}

#[test]
fn ingest_202_and_receipt_carry_the_same_flight_ticket() {
    let fixture = render_fixture_with(Arc::new(|_, _, _| {
        Ok(ReplayBatch {
            latest: EventSeq::new(0),
            events: vec![],
        })
    }));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut subscriber = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    subscriber
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let (server, _) = listener.accept().unwrap();
    fixture
        .state
        .streams
        .spawn(
            server,
            StreamRequest::parse(None, None, None).unwrap(),
            fixture.state.streams.reserve().unwrap(),
        )
        .unwrap();
    read_until(&mut subscriber, "\r\n\r\n");
    let reply = authed_post(&fixture, "/api/v1/ingest", &serde_json::json!({"api": BOARD_API}));
    assert!(reply.starts_with("HTTP/1.1 202 "), "{reply}");
    let body: serde_json::Value =
        serde_json::from_str(reply.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["ingest"], "queued");
    let ticket = body["ticket"]
        .as_str()
        .expect("the 202 carries the flight ticket");
    // The fixture has no router, so the relay publishes its failure receipt.
    let frame = read_until(&mut subscriber, "}\n\n");
    assert!(frame.contains("event: ingest\n"), "{frame}");
    let data = frame
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .unwrap();
    let receipt: serde_json::Value = serde_json::from_str(data).unwrap();
    assert_eq!(receipt["ticket"], ticket);
}

#[test]
fn completed_ingest_relay_reaches_stream_subscribers() {
    let fixture = render_fixture_with(Arc::new(|_, _, _| {
        Ok(ReplayBatch {
            latest: EventSeq::new(0),
            events: vec![],
        })
    }));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut subscriber = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    subscriber
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let (server, _) = listener.accept().unwrap();
    fixture
        .state
        .streams
        .spawn(
            server,
            StreamRequest::parse(None, None, None).unwrap(),
            fixture.state.streams.reserve().unwrap(),
        )
        .unwrap();
    read_until(&mut subscriber, "\r\n\r\n");
    let reply = authed_post(&fixture, "/api/v1/ingest", &serde_json::json!({"api": BOARD_API}));
    assert!(reply.starts_with("HTTP/1.1 202 "), "{reply}");
    // The fixture has no router, so the relay publishes its failure receipt.
    let frame = read_until(&mut subscriber, "event: ingest\n");
    assert!(frame.contains("\"error\""), "{frame}");
    assert!(frame.contains("board_unavailable"), "{frame}");
}

#[test]
fn render_plan_decodes_percent_encoded_revision_target() {
    let fixture = render_fixture();
    let plan = create_plan(&fixture);
    let revision = format!("\"revision\":\"{plan}@1\"");
    for target in [format!("{plan}@1"), format!("{plan}%401")] {
        let reply = render_get(&fixture, &format!("/api/v1/render/plan/{target}"));
        assert!(reply.starts_with("HTTP/1.1 200 "), "{target}: {reply}");
        assert!(reply.contains(&revision), "{target}: {reply}");
    }
}

#[test]
fn render_diff_decodes_percent_encoded_span_target() {
    let fixture = render_fixture();
    let plan = create_plan(&fixture);
    execute(
        &fixture,
        BoardOp::Edit {
            base: PlanRevision::new(plan, 1).unwrap(),
            body: PlanText::new("base\nrevised\n").unwrap(),
            summary: EntryText::new("revise the plan").unwrap(),
        },
    );
    for target in [format!("{plan}@1..2"), format!("{plan}%401..2")] {
        let reply = render_get(&fixture, &format!("/api/v1/render/diff/{target}"));
        assert!(reply.starts_with("HTTP/1.1 200 "), "{target}: {reply}");
        assert!(reply.contains("\"hunks\":"), "{target}: {reply}");
        assert!(reply.contains("revised"), "{target}: {reply}");
    }
}

#[test]
fn render_proposal_accepts_a_literal_entry_target() {
    let fixture = render_fixture();
    let plan = create_plan(&fixture);
    let proposal = fixture
        .state
        .store
        .with_writer(
            &fixture.config,
            Instant::now() + Duration::from_secs(5),
            |writer| {
                writer.handle(&BoardRequest::new(
                    fixture.config.actor(Some("codex"), Some("worker")).unwrap(),
                    BoardOp::Propose {
                        base: PlanRevision::new(plan, 1).unwrap(),
                        body: PlanText::new("proposed\n").unwrap(),
                        summary: EntryText::new("a proposal").unwrap(),
                        supersedes: None,
                    },
                ))
            },
        )
        .unwrap();
    let BoardResult::Change(proposal) = proposal.result else {
        panic!("expected proposal receipt")
    };
    let entry = proposal.entry.to_string();
    let reply = render_get(&fixture, &format!("/api/v1/render/proposal/{entry}"));
    assert!(reply.starts_with("HTTP/1.1 200 "), "{reply}");
    assert!(reply.contains(&format!("\"entry\":\"{entry}\"")), "{reply}");
}

#[test]
fn render_routes_reject_every_percent_escape_but_at() {
    let fixture = render_fixture();
    for path in [
        "/api/v1/render/plan/P1%4",
        "/api/v1/render/plan/P1%zz",
        "/api/v1/render/plan/P1%",
        "/api/v1/render/plan/P1%FF1",
        "/api/v1/render/diff/P1%4",
        "/api/v1/render/proposal/E%",
        "/api/v1/render/plan/P1%2f1",
        "/api/v1/render/plan/%2F",
        "/api/v1/render/plan/%00",
        "/api/v1/render/plan/P1%001",
        "/api/v1/render/plan/P1%411",
    ] {
        let reply = render_get(&fixture, path);
        assert!(reply.starts_with("HTTP/1.1 400 "), "{path}: {reply}");
        assert!(
            reply.contains("\"code\":\"invalid_options\""),
            "{path}: {reply}"
        );
    }
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

#[test]
fn challenge_route_answers_unauthenticated_with_a_verifiable_proof() {
    let fixture = render_fixture();
    let nonce = crate::board::board_web::web_guard::ChallengeNonce::generate().unwrap();
    let reply = challenge_post(
        &fixture,
        &serde_json::json!({"api": BOARD_API, "nonce": nonce.to_hex()}),
    );
    assert!(reply.starts_with("HTTP/1.1 200 "), "{reply}");
    assert!(!reply.contains(fixture.token.expose()), "{reply}");
    let json: serde_json::Value =
        serde_json::from_str(reply.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(json["api"], BOARD_API);
    let proof = json["proof"].as_str().unwrap();
    assert!(fixture.state.guard.challenge_matches(&nonce, proof));
    let other = BoardWebToken::rotate_at(&fixture._directory.path().join("other.token")).unwrap();
    let address: std::net::SocketAddr = fixture.authority.parse().unwrap();
    let foreign = WebGuard::with_token(address, other).unwrap();
    assert!(!foreign.challenge_matches(&nonce, proof));
}

#[test]
fn challenge_route_keeps_method_and_origin_checks_without_a_token() {
    let fixture = render_fixture();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    write!(
        client,
        "GET /api/v1/challenge HTTP/1.1\r\nHost: {}\r\n\r\n",
        fixture.authority
    )
    .unwrap();
    handle(server, Instant::now(), &fixture.state);
    let mut reply = String::new();
    client.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 405 "), "{reply}");
    let reply = challenge_post(
        &fixture,
        &serde_json::json!({"api": BOARD_API + 1, "nonce": "0".repeat(64)}),
    );
    assert!(reply.starts_with("HTTP/1.1 400 "), "{reply}");
    assert!(reply.contains("\"board_api_mismatch\""), "{reply}");
}

#[test]
fn stopped_filler_refuses_new_subscriptions_with_503() {
    let fixture = render_fixture_with(Arc::new(|_, _, _| panic!("feed poisoned")));
    // The filler stops itself after the panicking first fill; reserve()
    // must observe the stopped ring, not just the healthy poller wake.
    let started = Instant::now();
    while fixture.state.streams.reserve().is_ok() {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "reserve() never observed the stopped filler"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        fixture.state.streams.reserve().err().unwrap().code,
        BoardErrorCode::BoardUnavailable
    );
    let reply = render_get(&fixture, "/api/v1/events");
    assert!(reply.starts_with("HTTP/1.1 503 "), "{reply}");
    assert!(reply.contains("Retry-After: 1\r\n"), "{reply}");
}

#[test]
fn event_stream_rejects_an_unknown_plan_before_success() {
    let fixture = render_fixture_with(Arc::new(|_, plan, _| match plan {
        Some(plan) => Err(BoardError::new(
            BoardErrorCode::InvalidReference,
            format!("unknown plan {plan}"),
        )),
        None => Ok(ReplayBatch {
            latest: EventSeq::new(0),
            events: vec![],
        }),
    }));
    let reply = render_get(&fixture, "/api/v1/events?plan=P99999");
    assert!(reply.starts_with("HTTP/1.1 404 "), "{reply}");
    assert!(reply.contains("\"code\":\"invalid_reference\""), "{reply}");
    assert!(!reply.contains("HTTP/1.1 200"), "{reply}");
}

#[test]
fn private_routes_reject_wrong_methods_with_allow() {
    let fixture = render_fixture();
    for (method, path, allow) in [
        ("POST", "/api/v1/events", "GET"),
        ("GET", "/api/v1/board", "POST"),
        ("GET", "/api/v1/ingest", "POST"),
        ("POST", "/api/v1/render/plan/P1", "GET"),
        ("POST", "/api/v1/render/diff/P1@1..2", "GET"),
        ("POST", "/api/v1/render/proposal/E1", "GET"),
    ] {
        let reply = match method {
            "GET" => render_get(&fixture, path),
            _ => authed_post(&fixture, path, &serde_json::json!({"api": BOARD_API})),
        };
        assert!(
            reply.starts_with("HTTP/1.1 405 "),
            "{method} {path}: {reply}"
        );
        assert!(
            reply.contains(&format!("Allow: {allow}\r\n")),
            "{method} {path}: {reply}"
        );
    }
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

#[test]
fn misdirected_render_requests_carry_a_421_code() {
    let fixture = render_fixture();
    for path in [
        "/api/v1/render/plan/P1",
        "/api/v1/render/diff/P1@1..2",
        "/api/v1/render/proposal/E1",
    ] {
        let reply = wrong_host_get(&fixture, path);
        assert!(reply.starts_with("HTTP/1.1 421 "), "{path}: {reply}");
        assert!(
            reply.contains("\"code\":\"misdirected_request\""),
            "{path}: {reply}"
        );
    }
}

#[test]
fn slow_pre_auth_drip_is_cut_at_five_seconds_total_from_accept() {
    let fixture = render_fixture();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    let accepted_at = Instant::now();
    let mut dripper = client.try_clone().unwrap();
    let producer = std::thread::spawn(move || {
        dripper.write_all(b"G").unwrap();
        std::thread::sleep(Duration::from_secs(3));
        dripper.write_all(b"E").unwrap();
    });
    handle(server, accepted_at, &fixture.state);
    let mut reply = String::new();
    client.read_to_string(&mut reply).unwrap();
    producer.join().unwrap();
    assert!(reply.starts_with("HTTP/1.1 408 "), "{reply}");
    assert!(
        accepted_at.elapsed() < Duration::from_secs(7),
        "a second byte must not restart the five-second hold: {:?}",
        accepted_at.elapsed()
    );
}

#[test]
fn queue_full_refusal_delivers_the_busy_reply_before_closing() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    // A mid-request client: bytes queued unread server-side, no FIN yet.
    client
        .write_all(b"GET /api/v1/board HTTP/1.1\r\nHost: x")
        .unwrap();
    // The request must be queued unread server-side before the refusal, and
    // the reply must sit unread client-side before the read, so a reset
    // cannot slip past already-consumed bytes on either side.
    std::thread::sleep(Duration::from_millis(100));
    let busy = http_wire::unavailable_response(1, crate::board::board_web::QUEUE_FULL_BODY);
    crate::board::board_web::refuse_queue_full(server, &busy);
    std::thread::sleep(Duration::from_millis(100));
    // A reset may already have destroyed the connection; the body read below
    // is the assertion.
    let _ = client.shutdown(Shutdown::Write);
    let mut reply = Vec::new();
    client.read_to_end(&mut reply).unwrap();
    let reply = String::from_utf8(reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 503 "), "{reply}");
    assert!(reply.contains("daemon_busy"), "{reply}");
    assert!(reply.ends_with("}}"), "{reply}");
}
