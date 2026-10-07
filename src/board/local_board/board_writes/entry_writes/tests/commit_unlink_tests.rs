use super::*;
use crate::board::board_protocol::{BoardChange, BoardErrorCode};

pub(super) fn unlink_request(actor: BoardActor, task: TaskId) -> BoardRequest {
    let op = serde_json::from_value(serde_json::json!({
        "op": "unlink_commit", "oid": "b".repeat(40), "task": task.to_string()
    }))
    .expect("unlink_commit must be a supported wire operation");
    BoardRequest::new(actor, op)
}

pub(super) fn link_request(actor: BoardActor, task: TaskId) -> BoardRequest {
    let commit = manual_link_resolution();
    BoardRequest::new(
        actor,
        BoardOp::LinkCommit {
            oid: commit.oid,
            task,
            resolution: Some(Box::new(commit)),
        },
    )
}

pub(super) fn unlink_change(reply: BoardReply) -> BoardChange {
    let BoardResult::Change(change) = reply.result else {
        panic!("expected change: {reply:?}")
    };
    change
}

#[test]
fn unlink_removes_task_link_and_writes_event() {
    let directory = crate::board::board_test_support::scratch("board-unlink-");
    let mut board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    let (owner, task) = manual_link_plan(&mut board);
    board.handle(&link_request(owner.clone(), task)).unwrap();
    board
        .conn
        .execute("UPDATE commit_tasks SET link_seq=NULL", [])
        .unwrap();
    board
        .handle(&BoardRequest::new(
            owner.clone(),
            BoardOp::TaskCreate {
                plan: task.plan,
                title: PlanTitle::new("second repair").unwrap(),
                to: None,
                section: None,
            },
        ))
        .unwrap();
    let second = TaskId::new(task.plan, 2).unwrap();
    board.handle(&link_request(owner.clone(), second)).unwrap();
    let events = manual_link_count(&board, "SELECT COUNT(*) FROM events");
    let receipt = unlink_change(board.handle(&unlink_request(owner.clone(), task)).unwrap());
    assert!(!receipt.deduplicated);
    assert_eq!(receipt.task, Some(task));
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM commit_tasks"),
        1
    );
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM commit_plans"),
        1
    );
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM events"),
        events + 1
    );
    let (body, entry_kind, event_kind, user, harness): (String, String, String, String, String) =
        board
            .conn
            .query_row(
                concat!(
                    "SELECT e.body,e.kind,v.kind,a.user,a.harness FROM entries e ",
                    "JOIN events v ON v.seq=e.seq JOIN actors a ON a.id=v.actor_id WHERE e.id=?1"
                ),
                [sql_number(receipt.entry.get())],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
    assert_eq!(entry_kind, "unlinked");
    assert_eq!(event_kind, "unlinked");
    assert_eq!((user.as_str(), harness.as_str()), ("fixture", "human"));
    assert!(body.contains(&format!("unlinked {} from {task}", "b".repeat(40))));
    assert!(body.contains("source=manual"));
    assert!(body.contains("link_seq=unknown"));
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM entries WHERE kind='commit'"),
        1,
        "original commit evidence is retained"
    );
    board.handle(&unlink_request(owner, second)).unwrap();
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM commit_tasks"),
        0
    );
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM commit_plans"),
        0
    );
    assert_eq!(manual_link_count(&board, "SELECT COUNT(*) FROM commits"), 1);
}

#[test]
fn unlink_requires_owner_human_or_steward() {
    let directory = crate::board::board_test_support::scratch("board-unlink-");
    let mut board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    let (owner, task) = manual_link_plan(&mut board);
    board.handle(&link_request(owner.clone(), task)).unwrap();
    let denied = [
        manual_link_actor("fixture", "cli"),
        manual_link_actor("fixture", "claude"),
        manual_link_actor("other", "human"),
        manual_link_actor("other", "codex"),
    ];
    let events = manual_link_count(&board, "SELECT COUNT(*) FROM events");
    for actor in &denied {
        let error = board
            .handle(&unlink_request(actor.clone(), task))
            .unwrap_err();
        assert_eq!(
            error.code,
            BoardErrorCode::InvalidActor,
            "{actor:?}: {error}"
        );
    }
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM events"),
        events
    );
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM commit_tasks"),
        1
    );
    let steward = manual_link_actor("fixture", "codex");
    board.handle(&unlink_request(steward, task)).unwrap();
    for actor in denied {
        let error = board.handle(&unlink_request(actor, task)).unwrap_err();
        assert_eq!(
            error.code,
            BoardErrorCode::InvalidActor,
            "receipt replay is authorized"
        );
    }
    assert!(
        unlink_change(board.handle(&unlink_request(owner.clone(), task)).unwrap()).deduplicated
    );
    let unknown = TaskId::new(task.plan, 99).unwrap();
    let error = board.handle(&unlink_request(owner, unknown)).unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidReference);
}

#[test]
fn unlink_replay_is_idempotent() {
    let directory = crate::board::board_test_support::scratch("board-unlink-");
    let path = directory.path().join("board.sqlite3");
    let mut board = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    let (owner, task) = manual_link_plan(&mut board);
    board.handle(&link_request(owner.clone(), task)).unwrap();
    let first = unlink_change(board.handle(&unlink_request(owner.clone(), task)).unwrap());
    board
        .conn
        .execute("DELETE FROM operation_dedupes", [])
        .unwrap();
    let events = manual_link_count(&board, "SELECT COUNT(*) FROM events");
    drop(board);
    let mut reopened = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    let mut steward = manual_link_actor("fixture", "codex");
    steward.session = "different-session".into();
    let replay = unlink_change(reopened.handle(&unlink_request(steward, task)).unwrap());
    assert!(replay.deduplicated);
    assert_eq!((replay.entry, replay.seq), (first.entry, first.seq));
    assert_eq!(
        manual_link_count(&reopened, "SELECT COUNT(*) FROM events"),
        events
    );
    assert_eq!(
        manual_link_count(
            &reopened,
            "SELECT COUNT(*) FROM entries WHERE kind='unlinked'"
        ),
        1
    );
    reopened.handle(&link_request(owner.clone(), task)).unwrap();
    let fresh = unlink_change(reopened.handle(&unlink_request(owner, task)).unwrap());
    assert!(
        !fresh.deduplicated,
        "a relink starts a fresh unlink receipt"
    );
    assert_ne!(fresh.entry, first.entry);
    assert!(fresh.seq > first.seq);
    assert_eq!(
        manual_link_count(&reopened, "SELECT COUNT(*) FROM commit_tasks"),
        0
    );
    assert_eq!(
        manual_link_count(
            &reopened,
            "SELECT COUNT(*) FROM entries WHERE kind='unlinked'"
        ),
        2
    );
}
