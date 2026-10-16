use crate::board::board_actor::{BoardActor, BoardRecipient, HarnessLabel};
use crate::board::board_backend::BoardBackend;
use crate::board::board_ids::{PlanId, TaskId};
use crate::board::board_protocol::{
    AuthorityRole, BoardOp, BoardRequest, BoardResult, ClaimResume, WorkerAddress, WorkerHandle,
    WorkerRole,
};
use crate::board::board_vocabulary::{EntryText, PlanText, PlanTitle, TaskColumn};
use crate::board::local_board::LocalBoard;
use std::time::Duration;

fn board() -> (tempfile::TempDir, LocalBoard) {
    let directory = crate::board::board_test_support::scratch("task-move-authority-");
    let board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    (directory, board)
}

fn actor(harness: &str, session: &str) -> BoardActor {
    BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse(harness).unwrap(),
        session,
    )
    .unwrap()
}

fn change(
    reply: crate::board::board_protocol::BoardReply,
) -> crate::board::board_protocol::BoardChange {
    let BoardResult::Change(change) = reply.result else {
        panic!("expected change reply");
    };
    change
}

fn create_plan(board: &mut LocalBoard) -> PlanId {
    change(
        board
            .handle(&BoardRequest::new(
                actor("human", "owner"),
                BoardOp::New {
                    title: PlanTitle::new("Stale claim release").unwrap(),
                    body: PlanText::new("").unwrap(),
                    repo_key: None,
                },
            ))
            .unwrap(),
    )
    .plan
    .unwrap()
}

fn create_stale_claimed_task(board: &mut LocalBoard, plan: PlanId, title: &str) -> TaskId {
    let task = change(
        board
            .handle(&BoardRequest::new(
                actor("human", "owner"),
                BoardOp::TaskCreate {
                    plan,
                    title: PlanTitle::new(title).unwrap(),
                    to: None,
                    section: None,
                },
            ))
            .unwrap(),
    )
    .task
    .unwrap();
    board
        .handle(&BoardRequest::new(
            actor("codex", "holder"),
            BoardOp::ClaimTask {
                task,
                scope: Some(EntryText::new("work in progress").unwrap()),
                resume: ClaimResume::No,
                delegate: None,
            },
        ))
        .unwrap();
    board
        .conn
        .execute(
            "UPDATE claims SET last_active=0 WHERE plan_id=?1 AND task_ordinal=?2 AND ended_at IS NULL",
            rusqlite::params![plan.get() as i64, task.ordinal as i64],
        )
        .unwrap();
    task
}

fn create_live_claimed_task(board: &mut LocalBoard, plan: PlanId, title: &str) -> TaskId {
    let task = change(
        board
            .handle(&BoardRequest::new(
                actor("human", "owner"),
                BoardOp::TaskCreate {
                    plan,
                    title: PlanTitle::new(title).unwrap(),
                    to: None,
                    section: None,
                },
            ))
            .unwrap(),
    )
    .task
    .unwrap();
    board
        .handle(&BoardRequest::new(
            actor("codex", "holder"),
            BoardOp::ClaimTask {
                task,
                scope: Some(EntryText::new("live work").unwrap()),
                resume: ClaimResume::No,
                delegate: None,
            },
        ))
        .unwrap();
    task
}

fn task_column(board: &LocalBoard, task: TaskId) -> String {
    board
        .conn
        .query_row(
            "SELECT column_name FROM tasks WHERE plan_id=?1 AND ordinal=?2",
            rusqlite::params![task.plan.get() as i64, task.ordinal as i64],
            |row| row.get(0),
        )
        .unwrap()
}

fn open_claim_end_reason(board: &LocalBoard, task: TaskId) -> Option<String> {
    board
        .conn
        .query_row(
            "SELECT end_reason FROM claims WHERE plan_id=?1 AND task_ordinal=?2 ORDER BY id DESC LIMIT 1",
            rusqlite::params![task.plan.get() as i64, task.ordinal as i64],
            |row| row.get(0),
        )
        .unwrap()
}

fn claim_last_active(board: &LocalBoard, task: TaskId) -> i64 {
    board
        .conn
        .query_row(
            "SELECT last_active FROM claims WHERE plan_id=?1 AND task_ordinal=?2 AND ended_at IS NULL",
            rusqlite::params![task.plan.get() as i64, task.ordinal as i64],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn creating_an_assigned_task_requires_and_audits_assign_authority() {
    let (_directory, mut board) = board();
    let plan = create_plan(&mut board);
    let creator = actor("codex", "creator");
    let self_assignment = change(
        board
            .handle(&BoardRequest::new(
                creator.clone(),
                BoardOp::TaskCreate {
                    plan,
                    title: PlanTitle::new("Self-assigned task").unwrap(),
                    to: Some(BoardRecipient::for_actor(&creator)),
                    section: None,
                },
            ))
            .unwrap(),
    );
    let self_authority: Option<String> = board
        .conn
        .query_row(
            "SELECT authority_basis FROM entries WHERE id=?1",
            [i64::try_from(self_assignment.entry.get()).unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(self_authority, None);

    let before_entries: i64 = board
        .conn
        .query_row("SELECT count(*) FROM entries", [], |row| row.get(0))
        .unwrap();
    let error = board
        .handle(&BoardRequest::new(
            creator.clone(),
            BoardOp::TaskCreate {
                plan,
                title: PlanTitle::new("Assigned task").unwrap(),
                to: Some(BoardRecipient::parse("muse").unwrap()),
                section: None,
            },
        ))
        .unwrap_err();
    assert_eq!(
        error.code,
        crate::board::board_protocol::BoardErrorCode::InvalidActor
    );
    let task_recipient = TaskId::new(plan, 999).unwrap();
    let invalid_target = board
        .handle(&BoardRequest::new(
            actor("human", "owner"),
            BoardOp::TaskCreate {
                plan,
                title: PlanTitle::new("Task recipient rejected").unwrap(),
                to: Some(BoardRecipient::parse(&task_recipient.to_string()).unwrap()),
                section: None,
            },
        ))
        .unwrap_err();
    assert_eq!(
        invalid_target.code,
        crate::board::board_protocol::BoardErrorCode::InvalidOptions
    );
    let after_entries: i64 = board
        .conn
        .query_row("SELECT count(*) FROM entries", [], |row| row.get(0))
        .unwrap();
    assert_eq!(after_entries, before_entries);

    let grant = change(
        board
            .handle(&BoardRequest::new(
                actor("human", "owner"),
                BoardOp::Grant {
                    plan: Some(plan),
                    roles: vec![AuthorityRole::Assign],
                    grantee: BoardRecipient::parse("codex").unwrap(),
                    expires_in: None,
                    note: None,
                },
            ))
            .unwrap(),
    )
    .entry;
    let created = change(
        board
            .handle(&BoardRequest::new(
                creator,
                BoardOp::TaskCreate {
                    plan,
                    title: PlanTitle::new("Assigned task").unwrap(),
                    to: Some(BoardRecipient::parse("muse").unwrap()),
                    section: None,
                },
            ))
            .unwrap(),
    );
    let authority: (String, Option<i64>) = board
        .conn
        .query_row(
            "SELECT authority_basis,authority_entry FROM entries WHERE id=?1",
            [i64::try_from(created.entry.get()).unwrap()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(authority.0, "grant");
    assert_eq!(authority.1, Some(i64::try_from(grant.get()).unwrap()));
}

#[test]
fn stale_claims_only_release_to_todo_or_blocked_and_record_stale_authority() {
    let (_directory, mut board) = board();
    let plan = create_plan(&mut board);
    let mover = actor("muse", "stale-release");

    let unclaimed = change(
        board
            .handle(&BoardRequest::new(
                actor("codex", "creator"),
                BoardOp::TaskCreate {
                    plan,
                    title: PlanTitle::new("Creator has no move authority").unwrap(),
                    to: None,
                    section: None,
                },
            ))
            .unwrap(),
    )
    .task
    .unwrap();
    assert!(
        board
            .handle(&BoardRequest::new(
                actor("codex", "creator"),
                BoardOp::TaskMove {
                    task: unclaimed,
                    column: TaskColumn::Blocked,
                    to: None,
                },
            ))
            .is_err()
    );
    assert_eq!(task_column(&board, unclaimed), "todo");

    for column in [TaskColumn::Done, TaskColumn::Review] {
        let task = create_stale_claimed_task(&mut board, plan, "Protected completion");
        assert!(
            board
                .handle(&BoardRequest::new(
                    mover.clone(),
                    BoardOp::TaskMove {
                        task,
                        column,
                        to: None,
                    },
                ))
                .is_err()
        );
        assert_eq!(task_column(&board, task), "doing");
        assert_eq!(open_claim_end_reason(&board, task), None);
        assert_eq!(claim_last_active(&board, task), 0);
    }

    for column in [TaskColumn::Todo, TaskColumn::Blocked] {
        let task = create_stale_claimed_task(&mut board, plan, "Releasable work");
        board
            .handle(&BoardRequest::new(
                mover.clone(),
                BoardOp::TaskMove {
                    task,
                    column,
                    to: None,
                },
            ))
            .unwrap();
        assert_eq!(task_column(&board, task), column.as_str());
        assert_eq!(
            open_claim_end_reason(&board, task).as_deref(),
            Some("released")
        );
        let authority: Option<String> = board
            .conn
            .query_row(
                "SELECT authority_basis FROM events WHERE subject=?1 ORDER BY seq DESC LIMIT 1",
                [task.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(authority.as_deref(), Some("stale"));
    }
}

#[test]
fn release_grant_ends_live_other_claims_only_into_todo_or_blocked() {
    let (_directory, mut board) = board();
    let plan = create_plan(&mut board);
    let task = create_live_claimed_task(&mut board, plan, "Live claimed work");
    let releaser = actor("muse", "release");
    let denied = board
        .handle(&BoardRequest::new(
            releaser.clone(),
            BoardOp::TaskMove {
                task,
                column: TaskColumn::Todo,
                to: None,
            },
        ))
        .unwrap_err();
    assert_eq!(
        denied.code,
        crate::board::board_protocol::BoardErrorCode::InvalidActor
    );
    assert_eq!(task_column(&board, task), "doing");
    assert_eq!(open_claim_end_reason(&board, task), None);

    for column in [TaskColumn::Done, TaskColumn::Review] {
        let protected = create_live_claimed_task(&mut board, plan, "Protected live claim");
        assert!(
            board
                .handle(&BoardRequest::new(
                    releaser.clone(),
                    BoardOp::TaskMove {
                        task: protected,
                        column,
                        to: None,
                    },
                ))
                .is_err()
        );
        assert_eq!(task_column(&board, protected), "doing");
        assert_eq!(open_claim_end_reason(&board, protected), None);
    }

    let grant = change(
        board
            .handle(&BoardRequest::new(
                actor("human", "owner"),
                BoardOp::Grant {
                    plan: Some(plan),
                    roles: vec![AuthorityRole::Release],
                    grantee: BoardRecipient::parse("muse").unwrap(),
                    expires_in: None,
                    note: None,
                },
            ))
            .unwrap(),
    )
    .entry;
    for column in [TaskColumn::Done, TaskColumn::Review] {
        let protected = create_live_claimed_task(&mut board, plan, "Grant-protected live claim");
        assert!(
            board
                .handle(&BoardRequest::new(
                    actor("muse", "grant-protected"),
                    BoardOp::TaskMove {
                        task: protected,
                        column,
                        to: None,
                    },
                ))
                .is_err()
        );
        assert_eq!(task_column(&board, protected), "doing");
        assert_eq!(open_claim_end_reason(&board, protected), None);
    }
    board
        .handle(&BoardRequest::new(
            releaser,
            BoardOp::TaskMove {
                task,
                column: TaskColumn::Blocked,
                to: None,
            },
        ))
        .unwrap();
    assert_eq!(task_column(&board, task), "blocked");
    assert_eq!(
        open_claim_end_reason(&board, task).as_deref(),
        Some("released")
    );
    let authority: (String, Option<i64>) = board
        .conn
        .query_row(
            "SELECT authority_basis,authority_entry FROM events WHERE subject=?1 ORDER BY seq DESC LIMIT 1",
            [task.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(authority.0, "grant");
    assert_eq!(authority.1, Some(i64::try_from(grant.get()).unwrap()));
}

#[test]
fn bound_worker_can_release_its_stale_parent_claim_and_end_all_workers() {
    let (_directory, mut board) = board();
    let plan = create_plan(&mut board);
    let task = create_live_claimed_task(&mut board, plan, "Stale worker parent");
    let holder = actor("codex", "holder");
    let workers = [
        ("helper", WorkerRole::Code, actor("muse", "worker-one")),
        (
            "reviewer",
            WorkerRole::Review,
            actor("claude", "worker-two"),
        ),
    ];
    for (handle, role, worker) in &workers {
        let handle = WorkerHandle::parse(handle).unwrap();
        board
            .handle(&BoardRequest::new(
                holder.clone(),
                BoardOp::WorkerAdd {
                    task,
                    handle: handle.clone(),
                    role: *role,
                    model: "worker-model".to_owned(),
                    effort: Some("high".to_owned()),
                    scope: EntryText::new("implement a bounded helper").unwrap(),
                    files: vec![format!("src/{}.rs", handle.as_str())],
                },
            ))
            .unwrap();
        let address = WorkerAddress::new(task, handle).unwrap();
        board
            .handle(&BoardRequest::new(
                worker.clone(),
                BoardOp::Hello {
                    model: "worker-model".to_owned(),
                    effort: Some("high".to_owned()),
                    worker: Some(address),
                },
            ))
            .unwrap();
    }
    board
        .conn
        .execute(
            "UPDATE claims SET last_active=0 WHERE plan_id=?1 AND task_ordinal=?2 AND ended_at IS NULL",
            rusqlite::params![plan.get() as i64, task.ordinal as i64],
        )
        .unwrap();

    board
        .handle(&BoardRequest::new(
            workers[0].2.clone(),
            BoardOp::TaskMove {
                task,
                column: TaskColumn::Todo,
                to: None,
            },
        ))
        .unwrap();
    let ended: i64 = board
        .conn
        .query_row(
            "SELECT count(*) FROM claim_workers WHERE plan_id=?1 AND task_ordinal=?2 AND end_reason='claim_ended'",
            rusqlite::params![plan.get() as i64, task.ordinal as i64],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(ended, 2);
    let authority: Option<String> = board
        .conn
        .query_row(
            "SELECT authority_basis FROM events WHERE subject=?1 ORDER BY seq DESC LIMIT 1",
            [task.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(authority.as_deref(), Some("stale"));
}
