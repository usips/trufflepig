use super::*;
use crate::board::board_backend::BoardBackend;
use crate::board::board_web::event_stream::{EventStreams, SequencePoller};
use crate::board::board_web::web_guard::{BoardWebToken, WebGuard};
use crate::board::board_web::{WebStore, web_ops};
use crate::board::{
    board_config::{BoardConfig, BoardConfigCache},
    board_ids::{EventSeq, PlanId},
    board_protocol::{BoardReply, BoardRequest, BoardResult},
    board_vocabulary::{EntryText, PlanText, PlanTitle},
};
use std::io::{Read, Write};
use std::net::TcpListener;
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
    state: WebState,
    config: BoardConfig,
    token: BoardWebToken,
    authority: String,
}

fn render_fixture() -> RenderFixture {
    let directory = crate::board::board_test_support::scratch("web-render-");
    let config = BoardConfig::for_database(directory.path().join("web.sqlite3"));
    let store = WebStore::open_at(
        BoardConfigCache::with_config(config.clone()),
        directory.path().join("runtime"),
    )
    .unwrap();
    let token = BoardWebToken::load_at(&directory.path().join("board-web.token")).unwrap();
    let bind = TcpListener::bind("127.0.0.1:0").unwrap();
    let guard = WebGuard::with_token(bind.local_addr().unwrap(), token.clone()).unwrap();
    let authority = guard.authority().to_owned();
    let poller = SequencePoller::start(Arc::new(|| Ok(EventSeq::new(0)))).unwrap();
    let streams = EventStreams::new(
        Arc::new(|_, _, _| unreachable!("render routes never read the feed")),
        poller.handle(),
    );
    RenderFixture {
        _directory: directory,
        _poller: poller,
        state: WebState {
            store: Arc::new(store),
            guard,
            streams,
        },
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
    handle(server, Instant::now(), &fixture.state);
    let mut reply = String::new();
    client.read_to_string(&mut reply).unwrap();
    reply
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
fn render_proposal_decodes_percent_encoded_entry_target() {
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
    let digits = entry.strip_prefix('E').unwrap();
    let reply = render_get(&fixture, &format!("/api/v1/render/proposal/%45{digits}"));
    assert!(reply.starts_with("HTTP/1.1 200 "), "{reply}");
    assert!(reply.contains(&format!("\"entry\":\"{entry}\"")), "{reply}");
}

#[test]
fn render_routes_reject_malformed_percent_encoding() {
    let fixture = render_fixture();
    for path in [
        "/api/v1/render/plan/P1%4",
        "/api/v1/render/plan/P1%zz",
        "/api/v1/render/plan/P1%",
        "/api/v1/render/plan/P1%FF1",
        "/api/v1/render/diff/P1%4",
        "/api/v1/render/proposal/E%",
    ] {
        let reply = render_get(&fixture, path);
        assert!(reply.starts_with("HTTP/1.1 400 "), "{path}: {reply}");
        assert!(
            reply.contains("\"code\":\"invalid_options\""),
            "{path}: {reply}"
        );
    }
}

#[test]
fn render_routes_decode_slash_and_control_bytes_before_reference_parsing() {
    let fixture = render_fixture();
    for path in [
        "/api/v1/render/plan/P1%2f1",
        "/api/v1/render/plan/%2F",
        "/api/v1/render/plan/%00",
        "/api/v1/render/plan/P1%001",
    ] {
        let reply = render_get(&fixture, path);
        assert!(reply.starts_with("HTTP/1.1 400 "), "{path}: {reply}");
        assert!(
            reply.contains("\"code\":\"invalid_reference\""),
            "{path}: {reply}"
        );
    }
}
