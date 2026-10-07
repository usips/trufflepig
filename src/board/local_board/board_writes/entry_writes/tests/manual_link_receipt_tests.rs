use super::*;
use crate::board::board_protocol::{BoardChange, BoardReply};
use std::sync::{Arc, Barrier};

fn change(reply: BoardReply) -> BoardChange {
    let BoardResult::Change(change) = reply.result else {
        panic!("change result: {reply:?}")
    };
    change
}

fn manual_request(actor: BoardActor, task: TaskId) -> BoardRequest {
    BoardRequest::new(
        actor,
        BoardOp::LinkCommit {
            oid: crate::identity::GitOid::parse(&"b".repeat(40)).unwrap(),
            task,
            resolution: Some(Box::new(manual_link_resolution())),
        },
    )
}

#[test]
fn task_receipts_replay_their_own_events_across_callers_and_reopen() {
    let directory = crate::board::board_test_support::scratch("board-fixture-");
    let path = directory.path().join("board.sqlite3");
    let mut board = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    let (owner, first_task) = manual_link_plan(&mut board);
    board
        .handle(&BoardRequest::new(
            owner.clone(),
            BoardOp::TaskCreate {
                plan: first_task.plan,
                title: PlanTitle::new("follow-up").unwrap(),
                to: None,
                section: None,
            },
        ))
        .unwrap();
    let second_task = TaskId::new(first_task.plan, 2).unwrap();

    let first = change(
        board
            .handle(&manual_request(owner.clone(), first_task))
            .unwrap(),
    );
    let second_caller = manual_link_actor("fixture", "codex");
    let second = change(
        board
            .handle(&manual_request(second_caller.clone(), second_task))
            .unwrap(),
    );
    assert_eq!(first.seq, EventSeq::new(4));
    assert_eq!(second.seq, EventSeq::new(5));
    assert!(!first.deduplicated);
    assert_eq!(first.entry, second.entry, "the plan keeps one commit entry");
    assert!(!second.deduplicated);
    drop(board);

    let mut reopened = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    let events_before_replay = manual_link_count(&reopened, "SELECT COUNT(*) FROM events");
    let second_replay = change(
        reopened
            .handle(&manual_request(owner.clone(), second_task))
            .unwrap(),
    );
    assert!(second_replay.deduplicated);
    assert_eq!(second_replay.entry, second.entry);
    assert_eq!(second_replay.seq, second.seq);
    assert_eq!(second_replay.seq, EventSeq::new(5));
    assert_eq!(
        manual_link_count(&reopened, "SELECT COUNT(*) FROM events"),
        events_before_replay
    );

    let replay = change(
        reopened
            .handle(&manual_request(second_caller, first_task))
            .unwrap(),
    );
    assert!(replay.deduplicated);
    assert_eq!(replay.entry, first.entry);
    assert_eq!(replay.seq, EventSeq::new(4));
    assert_eq!(
        manual_link_count(&reopened, "SELECT COUNT(*) FROM events"),
        events_before_replay
    );
    assert_eq!(
        manual_link_count(
            &reopened,
            "SELECT COUNT(*) FROM commit_tasks WHERE source='manual' AND link_seq IS NOT NULL"
        ),
        2
    );
}

#[test]
fn concurrent_same_task_links_share_one_manual_receipt() {
    let directory = crate::board::board_test_support::scratch("board-fixture-");
    let path = directory.path().join("board.sqlite3");
    let mut setup = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    let (owner, task) = manual_link_plan(&mut setup);
    drop(setup);

    let mut first_board = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    let mut second_board = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    let start = Arc::new(Barrier::new(3));
    let first_start = Arc::clone(&start);
    let second_start = Arc::clone(&start);
    let first_request = manual_request(owner, task);
    let second_request = manual_request(manual_link_actor("fixture", "codex"), task);
    let first = std::thread::spawn(move || {
        first_start.wait();
        change(first_board.handle(&first_request).unwrap())
    });
    let second = std::thread::spawn(move || {
        second_start.wait();
        change(second_board.handle(&second_request).unwrap())
    });
    start.wait();

    let first = first.join().unwrap();
    let second = second.join().unwrap();
    assert_ne!(first.deduplicated, second.deduplicated);
    assert_eq!(first.entry, second.entry);
    assert_eq!(first.seq, second.seq);

    let reopened = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    assert_eq!(
        manual_link_count(&reopened, "SELECT COUNT(*) FROM events"),
        3
    );
    assert_eq!(
        manual_link_count(
            &reopened,
            "SELECT COUNT(*) FROM commit_tasks WHERE source='manual' AND link_seq IS NOT NULL"
        ),
        1
    );
}

#[test]
fn plan_only_scan_repair_reuses_entry_and_records_manual_task_event() {
    let directory = crate::board::board_test_support::scratch("board-fixture-");
    let mut board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    let (owner, task) = manual_link_plan(&mut board);
    let mut scanned = manual_link_resolution();
    scanned.plans = vec![CommitPlanLink {
        plan_id: task.plan,
        task_ordinal: None,
    }];
    board
        .handle(&BoardRequest::new(
            owner.clone(),
            BoardOp::LinkCommits {
                commits: vec![scanned],
            },
        ))
        .unwrap();
    let (scanned_entry, scanned_seq): (i64, i64) = board
        .conn
        .query_row(
            "SELECT p.entry_id,e.seq FROM commit_plans p JOIN entries e ON e.id=p.entry_id",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let events_before = manual_link_count(&board, "SELECT COUNT(*) FROM events");

    let linked = change(board.handle(&manual_request(owner, task)).unwrap());
    assert!(!linked.deduplicated);
    assert_eq!(linked.entry.get() as i64, scanned_entry);
    assert_ne!(linked.seq.get() as i64, scanned_seq);
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM entries WHERE kind='commit'"),
        1
    );
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM commit_plans"),
        1
    );
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM events"),
        events_before + 1
    );
    let (source, link_seq): (String, Option<i64>) = board
        .conn
        .query_row("SELECT source,link_seq FROM commit_tasks", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(source, "manual");
    assert_eq!(link_seq, Some(linked.seq.get() as i64));
}

#[test]
fn scan_task_link_keeps_scan_source_and_null_receipt() {
    let directory = crate::board::board_test_support::scratch("board-fixture-");
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
    board
        .handle(&BoardRequest::new(
            owner.clone(),
            BoardOp::LinkCommits {
                commits: vec![scanned],
            },
        ))
        .unwrap();
    let (scanned_entry, scanned_seq): (i64, i64) = board
        .conn
        .query_row(
            "SELECT p.entry_id,e.seq FROM commit_plans p JOIN entries e ON e.id=p.entry_id",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let events_before = manual_link_count(&board, "SELECT COUNT(*) FROM events");
    let replay = change(board.handle(&manual_request(owner, task)).unwrap());

    assert!(replay.deduplicated);
    assert_eq!(replay.entry.get() as i64, scanned_entry);
    assert_eq!(replay.seq.get() as i64, scanned_seq);
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM events"),
        events_before
    );
    assert_eq!(
        manual_link_count(
            &board,
            "SELECT COUNT(*) FROM commit_tasks WHERE source='scan' AND link_seq IS NULL"
        ),
        1
    );
}

#[test]
fn manual_link_with_unknown_historical_receipt_returns_invalid_state() {
    let directory = crate::board::board_test_support::scratch("board-fixture-");
    let mut board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    let (owner, task) = manual_link_plan(&mut board);
    let first = change(board.handle(&manual_request(owner.clone(), task)).unwrap());
    board
        .conn
        .execute(
            "UPDATE commit_tasks SET link_seq=NULL WHERE source='manual'",
            [],
        )
        .unwrap();
    let events_before = manual_link_count(&board, "SELECT COUNT(*) FROM events");

    let error = board
        .handle(&manual_request(manual_link_actor("fixture", "codex"), task))
        .unwrap_err();
    assert_eq!(
        error.code,
        crate::board::board_protocol::BoardErrorCode::InvalidState,
        "{error}"
    );
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM events"),
        events_before
    );
    assert_eq!(
        manual_link_count(
            &board,
            "SELECT COUNT(*) FROM commit_tasks WHERE source='manual' AND link_seq IS NULL"
        ),
        1
    );
    let matching_event: i64 = board
        .conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE seq=?1 AND summary=?2",
            rusqlite::params![
                first.seq.get() as i64,
                format!("linked {} to {task} by hand", "b".repeat(40))
            ],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(matching_event, 1, "the old matching event remains unknown");
}
