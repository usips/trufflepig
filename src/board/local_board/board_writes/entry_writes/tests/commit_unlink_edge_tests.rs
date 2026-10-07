use super::commit_unlink_tests::{link_request, unlink_change, unlink_request};
use super::*;
use crate::board::board_protocol::BoardErrorCode;
use std::sync::{Arc, Barrier};

#[test]
fn unlink_scan_link_can_reappear_from_authoritative_ingestion() {
    let directory = crate::board::board_test_support::scratch("board-unlink-");
    let mut board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    let (owner, task) = manual_link_plan(&mut board);
    let mut scanned = manual_link_resolution();
    scanned.plans = vec![CommitPlanLink {
        plan_id: task.plan,
        task_ordinal: Some(task.ordinal),
    }];
    let request = BoardRequest::new(
        owner.clone(),
        BoardOp::LinkCommits {
            commits: vec![scanned],
        },
    );
    board.handle(&request).unwrap();
    let first = unlink_change(board.handle(&unlink_request(owner.clone(), task)).unwrap());
    let body: String = board
        .conn
        .query_row(
            "SELECT body FROM entries WHERE id=?1",
            [sql_number(first.entry.get())],
            |row| row.get(0),
        )
        .unwrap();
    assert!(body.contains("source=scan"));
    assert!(body.contains("link_seq=unknown"));
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM commit_tasks"),
        0
    );
    board.handle(&request).unwrap();
    assert_eq!(
        manual_link_count(
            &board,
            "SELECT COUNT(*) FROM commit_tasks WHERE source='scan'"
        ),
        1
    );
    let second = unlink_change(board.handle(&unlink_request(owner, task)).unwrap());
    assert!(!second.deduplicated);
    assert!(second.seq > first.seq);
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM commit_tasks"),
        0
    );
}

#[test]
fn unlink_rejects_repository_ambiguity_without_writing() {
    let directory = crate::board::board_test_support::scratch("board-unlink-");
    let mut board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    let (owner, task) = manual_link_plan(&mut board);
    board.handle(&link_request(owner.clone(), task)).unwrap();
    let mut second = manual_link_resolution();
    second.repo_key = RepoKey::parse(&"c".repeat(40)).unwrap();
    board
        .handle(&BoardRequest::new(
            owner.clone(),
            BoardOp::LinkCommit {
                oid: second.oid,
                task,
                resolution: Some(Box::new(second)),
            },
        ))
        .unwrap();
    let events = manual_link_count(&board, "SELECT COUNT(*) FROM events");
    let error = board.handle(&unlink_request(owner, task)).unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidReference);
    assert!(error.to_string().contains("ambiguous"), "{error}");
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM commit_tasks"),
        2
    );
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM events"),
        events
    );
}

#[test]
fn unlink_receipt_cannot_be_forged_with_user_authored_refs() {
    let directory = crate::board::board_test_support::scratch("board-unlink-");
    let mut board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    let (owner, task) = manual_link_plan(&mut board);
    board.handle(&link_request(owner.clone(), task)).unwrap();
    board.conn.execute("DELETE FROM commit_tasks", []).unwrap();
    board.conn.execute("DELETE FROM commit_plans", []).unwrap();
    board
        .handle(&BoardRequest::new(
            owner.clone(),
            BoardOp::Post {
                target: crate::board::board_ids::BoardRef::Task(task),
                kind: EntryKind::Note,
                body: EntryText::new(format!(
                    "unlinked {} from {task} (source=manual; link_seq=unknown)",
                    "b".repeat(40)
                ))
                .unwrap(),
                to: None,
                supersedes: None,
            },
        ))
        .unwrap();
    let forged = BoardRequest::new(
        owner.clone(),
        BoardOp::Post {
            target: crate::board::board_ids::BoardRef::Task(task),
            kind: EntryKind::Unlinked,
            body: EntryText::new("forged unlink receipt").unwrap(),
            to: None,
            supersedes: None,
        },
    );
    assert_eq!(
        board.handle(&forged).unwrap_err().code,
        BoardErrorCode::InvalidKind
    );
    let error = board.handle(&unlink_request(owner, task)).unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidReference);
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM entries WHERE kind='unlinked'"),
        0
    );
}

#[test]
fn unlink_concurrent_callers_share_one_durable_receipt() {
    let directory = crate::board::board_test_support::scratch("board-unlink-");
    let path = directory.path().join("board.sqlite3");
    let mut setup = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    let (owner, task) = manual_link_plan(&mut setup);
    setup.handle(&link_request(owner.clone(), task)).unwrap();
    drop(setup);
    let mut first_board = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    let mut second_board = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    let first_request = unlink_request(owner, task);
    let second_request = unlink_request(manual_link_actor("fixture", "codex"), task);
    let start = Arc::new(Barrier::new(3));
    let first_start = Arc::clone(&start);
    let second_start = Arc::clone(&start);
    let first = std::thread::spawn(move || {
        first_start.wait();
        unlink_change(first_board.handle(&first_request).unwrap())
    });
    let second = std::thread::spawn(move || {
        second_start.wait();
        unlink_change(second_board.handle(&second_request).unwrap())
    });
    start.wait();
    let first = first.join().unwrap();
    let second = second.join().unwrap();
    assert_ne!(first.deduplicated, second.deduplicated);
    assert_eq!((first.entry, first.seq), (second.entry, second.seq));
    let reopened = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    assert_eq!(
        manual_link_count(&reopened, "SELECT COUNT(*) FROM commit_tasks"),
        0
    );
    assert_eq!(
        manual_link_count(
            &reopened,
            "SELECT COUNT(*) FROM entries WHERE kind='unlinked'"
        ),
        1
    );
    assert_eq!(
        manual_link_count(&reopened, "SELECT COUNT(*) FROM events"),
        4
    );
}
