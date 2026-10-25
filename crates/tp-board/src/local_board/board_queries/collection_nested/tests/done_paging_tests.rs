use super::*;
use crate::board::board_backend::BoardBackend;
use crate::board::board_protocol::ReadScope;
use crate::board::board_protocol::{BoardOp, BoardRequest};
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[test]
fn done_paging_is_recent_first() {
    let (_directory, mut board) = database();
    for (ordinal, seq) in [(1, 3), (2, 7), (3, 7), (4, 8)] {
        task(&board.conn, ordinal, seq);
        entry(&board.conn, ordinal + 10, seq, "task");
    }
    board
        .conn
        .execute("UPDATE tasks SET column_name='done' WHERE ordinal<4", [])
        .unwrap();
    let first = done_page(&mut board, None, 2);
    assert_eq!(task_ids(&first), vec!["P1.3", "P1.2"]);
    assert_eq!(first["next_before"], json!({ "seq": 7, "id": "P1.2" }));
    let second = done_page(&mut board, Some(first["next_before"].clone()), 2);
    assert_eq!(task_ids(&second), vec!["P1.1"]);
    assert_eq!(second["next_before"], Value::Null);
}

fn done_page(board: &mut LocalBoard, before: Option<Value>, limit: usize) -> Value {
    let op: BoardOp = serde_json::from_value(json!({
        "op": "tasks", "plan": "P1", "column": "done", "order": "recent_first",
        "before": before, "after": null, "ceiling": null, "through": null, "limit": limit,
    }))
    .expect("Tasks accepts completion order and its cursor");
    let reply = board
        .handle(&BoardRequest::new(context().actor, op))
        .unwrap();
    serde_json::to_value(reply).unwrap()["result"]["data"].clone()
}

fn task_ids(page: &Value) -> Vec<&str> {
    page["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|task| task["id"].as_str().unwrap())
        .collect()
}

#[test]
fn done_board_pages_break_shared_sequences_by_numeric_task_id() {
    let (_directory, mut board) = database();
    board.conn.execute_batch(
        "INSERT INTO plans(id,title,owner_user,head_revision,created_at) VALUES(10,'Ten','josh',1,1);
         INSERT INTO tasks VALUES(1,1,'First','done',NULL,NULL,7),(2,1,'Second','done',NULL,NULL,7),
         (10,1,'Tenth first','done',NULL,NULL,7),(10,2,'Tenth second','done',NULL,NULL,7);"
    ).unwrap();
    entry(&board.conn, 7, 7, "task");
    let mut seen = Vec::new();
    let mut before = None;
    loop {
        let page = board_done_page(&mut board, ReadScope::All, before, 1);
        seen.extend(task_ids(&page).into_iter().map(str::to_owned));
        before = page["next_before"]
            .as_object()
            .map(|_| page["next_before"].clone());
        if before.is_none() {
            break;
        }
    }
    assert_eq!(seen, vec!["P10.2", "P10.1", "P2.1", "P1.1"]);
}

#[test]
fn done_board_scope_filters_completed_tasks_without_duplicates() {
    let (_directory, mut board) = database();
    let a = RepoKey::parse(&"a".repeat(40)).unwrap();
    let b = RepoKey::parse(&"b".repeat(40)).unwrap();
    board
        .conn
        .execute(
            "INSERT INTO repos VALUES(?1,NULL),(?2,NULL)",
            params![a.as_str(), b.as_str()],
        )
        .unwrap();
    board
        .conn
        .execute(
            "INSERT INTO plan_repos VALUES(1,?1),(1,?2),(2,?2)",
            params![a.as_str(), b.as_str()],
        )
        .unwrap();
    board.conn.execute_batch(
        "INSERT INTO plans(id,title,owner_user,head_revision,created_at) VALUES(3,'Unscoped','josh',1,1);
         INSERT INTO tasks VALUES(1,1,'One','done',NULL,NULL,7),(2,1,'Two','done',NULL,NULL,7),
         (3,1,'Unscoped','done',NULL,NULL,7),(3,2,'Active','todo',NULL,NULL,8);"
    ).unwrap();
    entry(&board.conn, 7, 7, "task");
    entry(&board.conn, 8, 8, "task");
    let cases = [
        (ReadScope::Keys(BTreeSet::from([a.clone()])), vec!["P1.1"]),
        (
            ReadScope::Keys(BTreeSet::from([a.clone(), b])),
            vec!["P2.1", "P1.1"],
        ),
        (ReadScope::Unscoped, vec!["P3.1"]),
        (ReadScope::Repo(a), vec!["P3.1", "P1.1"]),
    ];
    for (scope, expected) in cases {
        let changes = board.conn.total_changes();
        board.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        let page = board_done_page(&mut board, scope, None, 10);
        board.conn.execute_batch("ROLLBACK").unwrap();
        assert_eq!(task_ids(&page), expected);
        assert_eq!(page["tasks"][0]["done_at"], 50);
        assert_eq!(
            board.conn.total_changes(),
            changes,
            "Done uses the query-only reader"
        );
    }
}

#[test]
fn done_recent_pages_drop_reopened_tasks_between_reads() {
    let (_directory, mut board) = database();
    for ordinal in 1..=3 {
        task(&board.conn, ordinal, ordinal + 2);
        entry(&board.conn, ordinal + 10, ordinal + 2, "task");
    }
    board
        .conn
        .execute("UPDATE tasks SET column_name='done'", [])
        .unwrap();
    let first = done_page(&mut board, None, 1);
    assert_eq!(task_ids(&first), vec!["P1.3"]);
    board
        .conn
        .execute(
            "UPDATE tasks SET column_name='todo',seq=6 WHERE ordinal=2",
            [],
        )
        .unwrap();
    entry(&board.conn, 16, 6, "task");
    let older = done_page(&mut board, Some(first["next_before"].clone()), 10);
    assert_eq!(task_ids(&older), vec!["P1.1"]);
    assert_eq!(older["omitted"], 0);
}

#[test]
fn done_recent_rejects_through_without_historical_task_versions() {
    let (_directory, mut board) = database();
    task(&board.conn, 1, 3);
    entry(&board.conn, 11, 3, "task");
    board
        .conn
        .execute("UPDATE tasks SET column_name='done'", [])
        .unwrap();
    let op: BoardOp = serde_json::from_value(json!({
        "op":"tasks", "plan":"P1", "column":"done", "order":"recent_first",
        "before":null, "after":null, "ceiling":null, "through":2, "limit":1,
    }))
    .unwrap();
    let error = board
        .handle(&BoardRequest::new(context().actor, op))
        .unwrap_err();
    assert_eq!(
        error.code,
        crate::board::board_protocol::BoardErrorCode::InvalidOptions
    );
    assert!(error.message.contains("through"));
}

#[test]
fn done_ordinal_filter_retains_the_original_ceiling() {
    let (_directory, mut board) = database();
    for ordinal in 1..=4 {
        task(&board.conn, ordinal, ordinal + 2);
        entry(&board.conn, ordinal + 10, ordinal + 2, "task");
    }
    board
        .conn
        .execute(
            "UPDATE tasks SET column_name='done' WHERE ordinal IN(1,3,4)",
            [],
        )
        .unwrap();
    let request = json!({ "op":"tasks", "plan":"P1", "column":"done", "order":"ordinal",
        "before":null,"after":null,"ceiling":null,"through":null,"limit":1 });
    let first = read_task_json(&mut board, request.clone());
    assert_eq!(task_ids(&first), vec!["P1.1"]);
    assert_eq!(first["omitted"], 2);
    task(&board.conn, 5, 7);
    board
        .conn
        .execute("UPDATE tasks SET column_name='done' WHERE ordinal=5", [])
        .unwrap();
    entry(&board.conn, 17, 7, "task");
    let mut continuation = request;
    continuation["after"] = first["next_after"].clone();
    continuation["ceiling"] = first["ceiling"].clone();
    continuation["through"] = first["through"].clone();
    continuation["limit"] = json!(10);
    assert_eq!(
        task_ids(&read_task_json(&mut board, continuation)),
        vec!["P1.3", "P1.4"]
    );
}

fn board_done_page(
    board: &mut LocalBoard,
    scope: ReadScope,
    before: Option<Value>,
    limit: usize,
) -> Value {
    read_task_json(
        board,
        json!({ "op":"done_tasks", "scope":scope, "before":before, "limit":limit }),
    )
}

fn read_task_json(board: &mut LocalBoard, value: Value) -> Value {
    let op: BoardOp = serde_json::from_value(value).unwrap();
    let reply = board
        .handle(&BoardRequest::new(context().actor, op))
        .unwrap();
    serde_json::to_value(reply).unwrap()["result"]["data"].clone()
}
