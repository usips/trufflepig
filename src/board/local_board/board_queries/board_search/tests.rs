use super::*;
use crate::board::board_actor::{BoardActor, HarnessLabel};
use crate::board::board_backend::BoardBackend;
use crate::board::board_ids::{EntryId, PlanRevision};
use crate::board::board_protocol::{BoardChange, BoardErrorCode, BoardOp, BoardRequest};
use crate::board::board_vocabulary::{EntryKind, EntryText, PlanText, PlanTitle};
use crate::board::local_board::LocalBoard;
use serde_json::{Value, json};
use std::time::Duration;

mod legacy_search_fixture;
mod search_migration;
mod search_queries;
mod search_transactions;

fn owner() -> BoardActor {
    BoardActor::new(
        "josh",
        "host",
        HarnessLabel::parse("human").unwrap(),
        "search-test",
    )
    .unwrap()
}

fn database() -> (tempfile::TempDir, LocalBoard) {
    let directory = crate::board::board_test_support::scratch("board-search-");
    let board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    (directory, board)
}

fn call(board: &mut LocalBoard, op: BoardOp) -> BoardChange {
    match board
        .handle(&BoardRequest::new(owner(), op))
        .unwrap()
        .result
    {
        BoardResult::Change(change) => change,
        other => panic!("unexpected {other:?}"),
    }
}

fn plan(board: &mut LocalBoard, title: &str, body: &str) -> PlanId {
    call(
        board,
        BoardOp::New {
            title: PlanTitle::new(title).unwrap(),
            body: PlanText::new(body).unwrap(),
            steward: None,
            repo_key: None,
        },
    )
    .plan
    .unwrap()
}

fn post(board: &mut LocalBoard, plan: PlanId, body: &str) -> EntryId {
    call(
        board,
        BoardOp::Post {
            target: BoardRef::Plan(plan),
            kind: EntryKind::Note,
            body: EntryText::new(body).unwrap(),
            to: None,
            supersedes: None,
        },
    )
    .entry
}

fn query_op(query: &str, plan: Option<PlanId>, limit: usize) -> BoardOp {
    serde_json::from_value(json!({"op":"search", "query":query, "plan":plan, "limit":limit}))
        .unwrap()
}

fn results(board: &mut LocalBoard, query: &str, plan: Option<PlanId>, limit: usize) -> Value {
    let reply = board
        .handle(&BoardRequest::new(owner(), query_op(query, plan, limit)))
        .unwrap();
    let result = serde_json::to_value(reply.result).unwrap();
    assert_eq!(result["result"], "search");
    result["data"].clone()
}

fn targets(result: &Value) -> std::collections::BTreeSet<String> {
    result["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["target"].as_str().unwrap().to_owned())
        .collect()
}

fn integrity(board: &LocalBoard) {
    for table in ["board_text", "plan_titles"] {
        board
            .conn
            .execute(
                &format!("INSERT INTO {table}({table},rank) VALUES('integrity-check',1)"),
                [],
            )
            .unwrap();
    }
}
