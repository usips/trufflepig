use super::super::*;
use crate::board::board_backend::BoardBackend;
use crate::board::board_ids::{BoardRef, EntryId, PlanId, TaskId};
use crate::board::board_protocol::{
    AuthorityRole, AuthoritySource, BoardErrorCode, BoardOp, BoardReply, BoardRequest, BoardResult,
    ClaimResume, ProgressInput, ProgressStep, WorkerAddress, WorkerEndReason, WorkerHandle,
    WorkerRole,
};
use crate::board::board_vocabulary::{EntryKind, EntryText, PlanText, PlanTitle};
use crate::{
    board::board_actor::{BoardActor, BoardRecipient},
    request_context::HarnessLabel,
};
use std::time::Duration;

fn actor(harness: &str, session: &str) -> BoardActor {
    BoardActor::new(
        "josh",
        "mutation-tests",
        HarnessLabel::parse(harness).unwrap(),
        session,
    )
    .unwrap()
}

fn database() -> (tempfile::TempDir, LocalBoard) {
    let directory = crate::test_fixtures::scratch("mutation-dispatch-");
    let path = directory.path().join("board.sqlite3");
    let board = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    (directory, board)
}

fn change(reply: BoardReply) -> crate::board::board_protocol::BoardChange {
    let BoardResult::Change(change) = reply.result else {
        panic!("expected mutation receipt");
    };
    change
}

fn create_plan(board: &mut LocalBoard) -> PlanId {
    change(
        board
            .handle(&BoardRequest::new(
                actor("human", "owner"),
                BoardOp::New {
                    title: PlanTitle::new("Mutation receipts").unwrap(),
                    body: PlanText::new("# Scope").unwrap(),
                    repo_key: None,
                },
            ))
            .unwrap(),
    )
    .plan
    .unwrap()
}

fn create_task(board: &mut LocalBoard, plan: PlanId, title: &str) -> TaskId {
    change(
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
    .unwrap()
}

fn claim_task(board: &mut LocalBoard, actor: BoardActor, task: TaskId, scope: &str) {
    board
        .handle(&BoardRequest::new(
            actor,
            BoardOp::ClaimTask {
                task,
                scope: Some(EntryText::new(scope).unwrap()),
                resume: ClaimResume::No,
                delegate: None,
            },
        ))
        .unwrap();
}

#[test]
fn same_holder_scope_update_preserves_claim_and_emits_no_event() {
    let (_directory, mut board) = database();
    let plan = create_plan(&mut board);
    let task = create_task(&mut board, plan, "Expanded scope");
    let claimant = actor("codex", "same-holder");
    claim_task(&mut board, claimant.clone(), task, "initial scope");

    let claim_row = |board: &LocalBoard| {
        board
            .conn
            .query_row(
                concat!(
                    "SELECT id,entry_id,claimed_at,scope FROM claims ",
                    "WHERE plan_id=?1 AND task_ordinal=?2 AND ended_at IS NULL"
                ),
                rusqlite::params![
                    i64::try_from(plan.get()).unwrap(),
                    i64::try_from(task.ordinal).unwrap()
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .unwrap()
    };
    let original_claim = claim_row(&board);
    let event_count: i64 = board
        .conn
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();
    let entry_count: i64 = board
        .conn
        .query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))
        .unwrap();

    let updated = change(
        board
            .handle(&BoardRequest::new(
                claimant,
                BoardOp::ClaimTask {
                    task,
                    scope: Some(EntryText::new("expanded scope").unwrap()),
                    resume: ClaimResume::No,
                    delegate: None,
                },
            ))
            .unwrap(),
    );

    assert!(!updated.deduplicated);
    assert_eq!(
        updated.entry.get(),
        u64::try_from(original_claim.1).unwrap()
    );
    assert_eq!(updated.plan, Some(plan));
    assert_eq!(updated.task, Some(task));
    let changed_claim = claim_row(&board);
    assert_eq!(changed_claim.0, original_claim.0);
    assert_eq!(changed_claim.1, original_claim.1);
    assert_eq!(changed_claim.2, original_claim.2);
    assert_eq!(changed_claim.3, "expanded scope");
    assert_eq!(
        board
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        event_count
    );
    assert_eq!(
        board
            .conn
            .query_row("SELECT COUNT(*) FROM entries", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        entry_count
    );
}

fn progress_post(actor: BoardActor, task: TaskId) -> BoardRequest {
    BoardRequest::new(
        actor,
        BoardOp::Post {
            target: BoardRef::Task(task),
            kind: EntryKind::Progress,
            body: EntryText::new("same structured update").unwrap(),
            to: None,
            supersedes: None,
            progress: Some(ProgressInput {
                head: None,
                step: Some(ProgressStep {
                    current: 1,
                    total: 3,
                }),
                tests: None,
                gates: Vec::new(),
                eta_secs: None,
            }),
            verdict: None,
        },
    )
}

fn task_activity(
    board: &LocalBoard,
    plan: PlanId,
    worker_task: TaskId,
    other_task: TaskId,
) -> (i64, i64, i64) {
    board
        .conn
        .query_row(
            concat!(
                "SELECT parent.last_active,worker.last_active,other.last_active FROM claims parent ",
                "JOIN claim_workers worker ON worker.claim_id=parent.id ",
                "JOIN claims other ON other.plan_id=parent.plan_id AND other.task_ordinal=?3 ",
                "WHERE parent.plan_id=?1 AND parent.task_ordinal=?2 AND parent.ended_at IS NULL ",
                "AND worker.handle='helper' AND worker.ended_at IS NULL AND other.ended_at IS NULL"
            ),
            rusqlite::params![
                i64::try_from(plan.get()).unwrap(),
                i64::try_from(worker_task.ordinal).unwrap(),
                i64::try_from(other_task.ordinal).unwrap()
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

#[test]
fn structured_post_receipt_is_scoped_to_the_current_claim_after_bye() {
    let (_directory, mut board) = database();
    let plan = create_plan(&mut board);
    let task = create_task(&mut board, plan, "Takeover");
    let claimant = actor("codex", "same-session");
    claim_task(&mut board, claimant.clone(), task, "implement progress");

    let request = progress_post(claimant.clone(), task);
    let first = change(board.handle(&request).unwrap());
    let repeated = change(board.handle(&request).unwrap());
    assert!(repeated.deduplicated);
    assert_eq!(repeated.entry, first.entry);

    board
        .handle(&BoardRequest::new(claimant.clone(), BoardOp::Bye))
        .unwrap();
    let ended_at: Option<i64> = board
        .conn
        .query_row(
            "SELECT s.ended_at FROM agent_sessions s JOIN actors a ON a.id=s.actor_id WHERE a.session=?1",
            ["same-session"],
            |row| row.get(0),
        )
        .unwrap();
    assert!(ended_at.is_some());
    board
        .handle(&BoardRequest::new(
            claimant.clone(),
            BoardOp::Show {
                target: BoardRef::Task(task),
            },
        ))
        .unwrap();
    let still_ended: Option<i64> = board
        .conn
        .query_row(
            "SELECT s.ended_at FROM agent_sessions s JOIN actors a ON a.id=s.actor_id WHERE a.session=?1",
            ["same-session"],
            |row| row.get(0),
        )
        .unwrap();
    assert!(still_ended.is_some());

    claim_task(&mut board, claimant.clone(), task, "take over progress");
    let revived: Option<i64> = board
        .conn
        .query_row(
            "SELECT s.ended_at FROM agent_sessions s JOIN actors a ON a.id=s.actor_id WHERE a.session=?1",
            ["same-session"],
            |row| row.get(0),
        )
        .unwrap();
    assert!(revived.is_none());

    let takeover = change(board.handle(&request).unwrap());
    assert!(!takeover.deduplicated);
    assert_ne!(takeover.entry, first.entry);
    let mut statement = board
        .conn
        .prepare(
            "SELECT DISTINCT p.claim_id FROM progress_facts p WHERE p.entry_id IN (?1,?2) ORDER BY p.claim_id",
        )
        .unwrap();
    let claim_ids: Vec<i64> = statement
        .query_map(
            rusqlite::params![
                i64::try_from(first.entry.get()).unwrap(),
                i64::try_from(takeover.entry.get()).unwrap()
            ],
            |row| row.get(0),
        )
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(claim_ids.len(), 2);
}

#[test]
fn bound_worker_post_refreshes_only_its_parent_claim() {
    let (_directory, mut board) = database();
    let plan = create_plan(&mut board);
    let worker_task = create_task(&mut board, plan, "Worker task");
    let other_task = create_task(&mut board, plan, "Lead task");
    let lead = actor("codex", "lead");
    let worker = actor("codex", "worker");
    claim_task(
        &mut board,
        lead.clone(),
        worker_task,
        "delegate implementation",
    );

    let handle = WorkerHandle::parse("helper").unwrap();
    board
        .handle(&BoardRequest::new(
            lead,
            BoardOp::WorkerAdd {
                task: worker_task,
                handle: handle.clone(),
                role: WorkerRole::Code,
                model: "worker-model".to_owned(),
                effort: Some("high".to_owned()),
                scope: EntryText::new("implement the task").unwrap(),
                files: vec!["src/example.rs".to_owned()],
            },
        ))
        .unwrap();
    board
        .handle(&BoardRequest::new(
            worker.clone(),
            BoardOp::Hello {
                model: "worker-model".to_owned(),
                effort: Some("high".to_owned()),
                worker: Some(WorkerAddress::new(worker_task, handle.clone()).unwrap()),
            },
        ))
        .unwrap();
    claim_task(&mut board, worker.clone(), other_task, "own lead work");

    board
        .conn
        .execute(
            "UPDATE claims SET last_active=2 WHERE plan_id=?1 AND task_ordinal=?2 AND ended_at IS NULL",
            rusqlite::params![
                i64::try_from(plan.get()).unwrap(),
                i64::try_from(worker_task.ordinal).unwrap()
            ],
        )
        .unwrap();
    board
        .conn
        .execute(
            "UPDATE claim_workers SET last_active=1 WHERE plan_id=?1 AND task_ordinal=?2 AND handle='helper' AND ended_at IS NULL",
            rusqlite::params![
                i64::try_from(plan.get()).unwrap(),
                i64::try_from(worker_task.ordinal).unwrap()
            ],
        )
        .unwrap();
    board
        .conn
        .execute(
            "UPDATE claims SET last_active=3 WHERE plan_id=?1 AND task_ordinal=?2 AND ended_at IS NULL",
            rusqlite::params![
                i64::try_from(plan.get()).unwrap(),
                i64::try_from(other_task.ordinal).unwrap()
            ],
        )
        .unwrap();

    let post = BoardRequest::new(
        worker,
        BoardOp::Post {
            target: BoardRef::Task(worker_task),
            kind: EntryKind::Progress,
            body: EntryText::new("worker update").unwrap(),
            to: None,
            supersedes: None,
            progress: None,
            verdict: None,
        },
    );
    board.handle(&post).unwrap();
    let activity = task_activity(&board, plan, worker_task, other_task);
    assert!(activity.0 > 3);
    assert!(activity.1 > 3);
    assert_eq!(activity.2, 3);

    board
        .conn
        .execute(
            "UPDATE claims SET last_active=2 WHERE plan_id=?1 AND task_ordinal=?2 AND ended_at IS NULL",
            rusqlite::params![
                i64::try_from(plan.get()).unwrap(),
                i64::try_from(worker_task.ordinal).unwrap()
            ],
        )
        .unwrap();
    board
        .conn
        .execute(
            "UPDATE claim_workers SET last_active=1 WHERE plan_id=?1 AND task_ordinal=?2 AND handle='helper' AND ended_at IS NULL",
            rusqlite::params![
                i64::try_from(plan.get()).unwrap(),
                i64::try_from(worker_task.ordinal).unwrap()
            ],
        )
        .unwrap();
    board
        .conn
        .execute(
            "UPDATE claims SET last_active=3 WHERE plan_id=?1 AND task_ordinal=?2 AND ended_at IS NULL",
            rusqlite::params![
                i64::try_from(plan.get()).unwrap(),
                i64::try_from(other_task.ordinal).unwrap()
            ],
        )
        .unwrap();
    let replay = change(board.handle(&post).unwrap());
    assert!(replay.deduplicated);
    let retry_activity = task_activity(&board, plan, worker_task, other_task);
    assert!(retry_activity.0 > 3);
    assert!(retry_activity.1 > 3);
    assert_eq!(retry_activity.2, 3);
}

fn grant_request(plan: PlanId, grantee: &BoardActor, expires_in: Option<u64>) -> BoardRequest {
    BoardRequest::new(
        actor("human", "owner"),
        BoardOp::Grant {
            plan: Some(plan),
            roles: vec![AuthorityRole::Triage],
            grantee: BoardRecipient::for_actor(grantee),
            expires_in,
            note: None,
        },
    )
}

fn live_triage_grant(board: &LocalBoard, plan: PlanId, grantee: &BoardActor) -> Option<EntryId> {
    let now = unix_now().unwrap();
    match super::super::board_authority::authority_for(
        &board.conn,
        grantee,
        Some(plan),
        AuthorityRole::Triage,
        now,
    )
    .unwrap()
    {
        Some(AuthoritySource::Grant(entry)) => Some(entry),
        _ => None,
    }
}

#[test]
fn revoked_grant_does_not_replay_for_an_identical_regrant() {
    let (_directory, mut board) = database();
    let plan = create_plan(&mut board);
    let codex = actor("codex", "s1");
    let request = grant_request(plan, &codex, None);
    let first = change(board.handle(&request).unwrap()).entry;
    board
        .handle(&BoardRequest::new(
            actor("human", "owner"),
            BoardOp::Revoke { entry: first },
        ))
        .unwrap();
    assert_eq!(live_triage_grant(&board, plan, &codex), None);

    let regrant = change(board.handle(&request).unwrap());
    assert!(!regrant.deduplicated);
    assert_ne!(regrant.entry, first);
    assert_eq!(live_triage_grant(&board, plan, &codex), Some(regrant.entry));
}

#[test]
fn expired_grant_does_not_replay_for_an_identical_regrant() {
    let (_directory, mut board) = database();
    let plan = create_plan(&mut board);
    let codex = actor("codex", "s1");
    let request = grant_request(plan, &codex, Some(300));
    let first = change(board.handle(&request).unwrap()).entry;
    // There is no clock seam: moving the stored expiry into the past models
    // time passing while the ten-minute dedupe row stays inside its window.
    board
        .conn
        .execute(
            "UPDATE authority_grants SET expires_at=?2 WHERE entry_id=?1",
            rusqlite::params![i64::try_from(first.get()).unwrap(), unix_now().unwrap() - 1],
        )
        .unwrap();
    assert_eq!(live_triage_grant(&board, plan, &codex), None);

    let regrant = change(board.handle(&request).unwrap());
    assert!(!regrant.deduplicated);
    assert_ne!(regrant.entry, first);
    assert_eq!(live_triage_grant(&board, plan, &codex), Some(regrant.entry));
}

#[test]
fn worker_add_does_not_replay_after_its_worker_ends() {
    let (_directory, mut board) = database();
    let plan = create_plan(&mut board);
    let task = create_task(&mut board, plan, "Ended worker");
    let lead = actor("codex", "lead");
    claim_task(&mut board, lead.clone(), task, "delegate implementation");
    let handle = WorkerHandle::parse("helper").unwrap();
    let add = BoardRequest::new(
        lead.clone(),
        BoardOp::WorkerAdd {
            task,
            handle: handle.clone(),
            role: WorkerRole::Code,
            model: "worker-model".to_owned(),
            effort: Some("high".to_owned()),
            scope: EntryText::new("implement the task").unwrap(),
            files: vec!["src/example.rs".to_owned()],
        },
    );
    board.handle(&add).unwrap();
    board
        .handle(&BoardRequest::new(
            lead,
            BoardOp::WorkerEnd {
                worker: WorkerAddress::new(task, handle).unwrap(),
                state: WorkerEndReason::Done,
                note: None,
            },
        ))
        .unwrap();
    let error = board.handle(&add).unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidWorker);
}
