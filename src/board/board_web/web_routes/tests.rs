use super::*;
use crate::board::board_backend::BoardBackend;
use crate::board::board_web::{WebStore, web_ops};
use crate::board::{
    board_config::{BoardConfig, BoardConfigCache},
    board_protocol::{BoardRequest, BoardResult},
    board_vocabulary::{EntryText, PlanText, PlanTitle},
};
use std::io::Read;
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
