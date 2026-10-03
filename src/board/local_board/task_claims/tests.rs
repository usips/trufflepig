use std::path::PathBuf;
use std::sync::{Arc, Barrier};

use super::*;
use crate::board::board_actor::{BoardActor, HarnessLabel};
use crate::board::board_backend::BoardBackend;
use crate::board::board_protocol::{BoardOp, BoardRequest};
use crate::board::local_board::LocalBoard;

struct ClaimDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl ClaimDatabase {
    fn new() -> Self {
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/task-claim-tests")
            .join(uuid::Uuid::new_v4().to_string());
        let path = directory.join("board.sqlite3");
        let database = Self { directory, path };
        let conn = database.connect();
        conn.execute("INSERT INTO plans(id,title,owner_user,steward,head_revision,next_task,created_at) VALUES(1,'Plan','josh','claude',1,0,100)", []).unwrap();
        database
    }

    fn connect(&self) -> Connection {
        super::super::board_database::open(&self.path).unwrap().0
    }
}

impl Drop for ClaimDatabase {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn actor(user: &str, harness: &str, session: &str) -> BoardActor {
    BoardActor::new(
        user,
        "test-host",
        HarnessLabel::parse(harness).unwrap(),
        session,
    )
    .unwrap()
}

fn write(
    conn: &mut Connection,
    actor: &BoardActor,
    now: i64,
    operation: impl FnOnce(&Transaction<'_>, &WriteContext) -> Result<BoardReply, BoardError>,
) -> Result<BoardReply, BoardError> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let actor_id = super::super::ensure_actor(&tx, actor, now).unwrap();
    let ctx = WriteContext {
        actor_id,
        actor: actor.clone(),
        model: Some("test-model".into()),
        effort: Some("xhigh".into()),
        now,
        seq: EventSeq::new(super::super::max_seq(&tx).unwrap().get() + 1),
        claim_ttl_secs: 120,
    };
    let reply = operation(&tx, &ctx)?;
    tx.commit().unwrap();
    Ok(reply)
}

fn carved_task(conn: &mut Connection, actor: &BoardActor, now: i64, scope: &str) -> TaskId {
    let reply = write(conn, actor, now, |tx, ctx| {
        carve_claim(
            tx,
            ctx,
            PlanId::new(1).unwrap(),
            &PlanTitle::new("Claim lane").unwrap(),
            &EntryText::new(scope).unwrap(),
            Some("Claims"),
        )
    })
    .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("expected mutation reply");
    };
    change.task.unwrap()
}

fn claim(
    conn: &mut Connection,
    actor: &BoardActor,
    now: i64,
    task: TaskId,
    scope: &str,
) -> Result<BoardReply, BoardError> {
    write(conn, actor, now, |tx, ctx| {
        claim_task(tx, ctx, task, &EntryText::new(scope).unwrap())
    })
}

#[test]
fn second_connection_conflict_names_snapshot_and_activity() {
    let database = ClaimDatabase::new();
    let mut first = database.connect();
    let mut second = database.connect();
    let holder = actor("josh", "codex", "one");
    let task = carved_task(&mut first, &holder, 1000, "parser and tests");
    first
        .execute(
            "UPDATE agent_sessions SET model='replacement',effort='low'",
            [],
        )
        .unwrap();
    let error = claim(
        &mut second,
        &actor("josh", "muse", "two"),
        1100,
        task,
        "different lane",
    )
    .unwrap_err()
    .to_string();
    assert!(error.starts_with("claim_conflict:"), "{error}");
    assert!(error.contains(&holder.identity()));
    assert!(error.contains("test-model/xhigh"));
    assert!(error.contains("since 1000, active 100s ago (last activity 1000)"));
    assert_eq!(read_claims(&first, task.plan, 1100, 120).unwrap().len(), 1);
}

#[test]
fn stale_takeover_preserves_scope_and_targets_previous_holder() {
    let database = ClaimDatabase::new();
    let mut first = database.connect();
    let mut second = database.connect();
    let old = actor("josh", "codex", "one");
    let new = actor("josh", "muse", "two");
    let task = carved_task(&mut first, &old, 1000, "old scope");
    assert!(!read_claims(&first, task.plan, 1120, 120).unwrap()[0].stale);
    assert!(claim(&mut second, &new, 1120, task, "new scope").is_err());
    assert!(read_claims(&first, task.plan, 1121, 120).unwrap()[0].stale);
    claim(&mut second, &new, 1121, task, "new scope").unwrap();
    let claims = read_claims(&first, task.plan, 1121, 120).unwrap();
    assert_eq!(claims.len(), 2);
    assert_eq!(claims[0].scope.as_str(), "old scope");
    assert_eq!(claims[0].ended_at, Some(1121));
    assert_eq!(claims[0].end_reason, Some(ClaimEndReason::TakenOver));
    assert_eq!(claims[1].actor, new);
    assert_eq!(claims[1].scope.as_str(), "new scope");
    let recipient: String = first
        .query_row(
            "SELECT to_whom FROM events ORDER BY seq DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(recipient, old.identity());
    let summary: String = first
        .query_row(
            "SELECT summary FROM events ORDER BY seq DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(summary.contains("took over stale claim from"));
}

#[test]
fn simultaneous_carves_allocate_distinct_ordinals_and_sections() {
    let database = ClaimDatabase::new();
    let first = database.connect();
    let second = database.connect();
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = [first, second]
        .into_iter()
        .enumerate()
        .map(|(index, mut conn)| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                carved_task(
                    &mut conn,
                    &actor("josh", "codex", &format!("session-{index}")),
                    1000,
                    "exclusive carve",
                )
            })
        })
        .collect();
    let mut ordinals: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap().ordinal)
        .collect();
    ordinals.sort_unstable();
    assert_eq!(ordinals, [1, 2]);
    let conn = database.connect();
    let tasks = read_tasks(&conn, PlanId::new(1).unwrap()).unwrap();
    assert!(
        tasks
            .iter()
            .all(|task| task.section.as_deref() == Some("Claims")
                && task.column == TaskColumn::Doing)
    );
    assert_eq!(
        read_claims(&conn, PlanId::new(1).unwrap(), 1000, 120)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn simultaneous_claimants_leave_one_active_lease() {
    let database = ClaimDatabase::new();
    let mut setup = database.connect();
    let task = carved_task(
        &mut setup,
        &actor("josh", "codex", "original"),
        1000,
        "initial",
    );
    let first = database.connect();
    let second = database.connect();
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = [first, second]
        .into_iter()
        .enumerate()
        .map(|(index, mut conn)| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                claim(
                    &mut conn,
                    &actor("josh", "muse", &format!("new-{index}")),
                    1121,
                    task,
                    "replacement",
                )
            })
        })
        .collect();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .all(|error| error.to_string().starts_with("claim_conflict:"))
    );
    let live: i64 = setup
        .query_row(
            "SELECT count(*) FROM claims WHERE ended_at IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(live, 1);
}

#[test]
fn failed_carve_rolls_back_task_ordinal_and_claim() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    conn.execute_batch("CREATE TRIGGER refuse_claim_event BEFORE INSERT ON events WHEN NEW.kind='claim' BEGIN SELECT RAISE(ABORT,'test failure'); END;").unwrap();
    let result = write(
        &mut conn,
        &actor("josh", "codex", "one"),
        1000,
        |tx, ctx| {
            carve_claim(
                tx,
                ctx,
                PlanId::new(1).unwrap(),
                &PlanTitle::new("Atomic").unwrap(),
                &EntryText::new("scope").unwrap(),
                Some("Heading"),
            )
        },
    );
    assert!(result.is_err());
    let ordinal: i64 = conn
        .query_row("SELECT next_task FROM plans WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(ordinal, 0);
    assert!(
        read_tasks(&conn, PlanId::new(1).unwrap())
            .unwrap()
            .is_empty()
    );
    assert!(
        read_claims(&conn, PlanId::new(1).unwrap(), 1000, 120)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn ordinary_activity_refreshes_only_callers_live_claims() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "codex", "one");
    let peer = actor("josh", "muse", "two");
    let own = carved_task(&mut conn, &holder, 1000, "own");
    let other = carved_task(&mut conn, &peer, 1000, "other");
    write(&mut conn, &holder, 1050, |tx, ctx| {
        refresh_plan_claims(tx, ctx.actor_id, own.plan, ctx.now)?;
        change(ctx, EntryId::new(1).unwrap(), own)
    })
    .unwrap();
    let claims = read_claims(&conn, own.plan, 1050, 120).unwrap();
    assert_eq!(claims[0].last_active, 1050);
    assert_eq!(claims[1].last_active, 1000);
    write(&mut conn, &holder, 1080, |tx, ctx| {
        refresh_inbox_claims(tx, ctx.actor_id, ctx.now)?;
        change(ctx, EntryId::new(1).unwrap(), own)
    })
    .unwrap();
    assert_eq!(
        read_claims(&conn, other.plan, 1080, 120).unwrap()[0].last_active,
        1080
    );
    write(&mut conn, &holder, 1090, |tx, ctx| {
        move_task(tx, ctx, own, TaskColumn::Review, None)
    })
    .unwrap();
    write(&mut conn, &holder, 1200, |tx, ctx| {
        refresh_inbox_claims(tx, ctx.actor_id, ctx.now)?;
        change(ctx, EntryId::new(1).unwrap(), own)
    })
    .unwrap();
    assert_eq!(
        read_claims(&conn, own.plan, 1200, 120).unwrap()[0].last_active,
        1080
    );
}

#[test]
fn commit_activity_requires_current_task_matching_coauthor_and_time() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "codex", "one");
    let task = carved_task(&mut conn, &holder, 1000, "current");
    let coauthor = CommitCoauthor {
        harness: HarnessLabel::parse("codex").unwrap(),
        model: "Codex".into(),
        email: "agent@openai.com".into(),
    };
    let wrong = CommitCoauthor {
        harness: HarnessLabel::parse("muse").unwrap(),
        ..coauthor.clone()
    };
    for (coauthors, committed_at, now) in [
        (&[coauthor.clone()][..], 999, 1100),
        (&[wrong][..], 1050, 1100),
        (&[coauthor.clone()][..], 1200, 1100),
    ] {
        write(&mut conn, &holder, now, |tx, ctx| {
            refresh_commit_claims(tx, task.plan, task.ordinal, coauthors, committed_at, now)?;
            change(ctx, EntryId::new(1).unwrap(), task)
        })
        .unwrap();
    }
    assert_eq!(
        read_claims(&conn, task.plan, 1100, 120).unwrap()[0].last_active,
        1000
    );
    write(&mut conn, &holder, 1100, |tx, ctx| {
        refresh_commit_claims(tx, task.plan, task.ordinal, &[coauthor.clone()], 1050, 1100)?;
        change(ctx, EntryId::new(1).unwrap(), task)
    })
    .unwrap();
    assert_eq!(
        read_claims(&conn, task.plan, 1100, 120).unwrap()[0].last_active,
        1100
    );
    write(&mut conn, &holder, 1110, |tx, ctx| {
        move_task(tx, ctx, task, TaskColumn::Done, None)
    })
    .unwrap();
    write(&mut conn, &holder, 1150, |tx, ctx| {
        refresh_commit_claims(tx, task.plan, task.ordinal, &[coauthor], 1120, 1150)?;
        change(ctx, EntryId::new(1).unwrap(), task)
    })
    .unwrap();
    assert_eq!(
        read_claims(&conn, task.plan, 1150, 120).unwrap()[0].last_active,
        1100
    );
}

#[test]
fn other_actor_cannot_release_or_reassign_held_card() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let task = carved_task(&mut conn, &actor("josh", "codex", "one"), 1000, "exclusive");
    let recipient = BoardRecipient::parse("muse").unwrap();
    for unauthorized in [
        actor("josh", "muse", "two"),
        actor("other", "human", "web"),
        actor("other", "claude", "three"),
    ] {
        for (column, to) in [
            (TaskColumn::Review, None),
            (TaskColumn::Doing, Some(&recipient)),
        ] {
            let error = write(&mut conn, &unauthorized, 1050, |tx, ctx| {
                move_task(tx, ctx, task, column, to)
            })
            .unwrap_err();
            assert!(error.to_string().starts_with("invalid_actor:"));
        }
    }
    assert!(
        read_claims(&conn, task.plan, 1050, 120).unwrap()[0]
            .ended_at
            .is_none()
    );
}

#[test]
fn owner_steward_reassignment_ends_claim_and_assigns_recipient() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let task = carved_task(&mut conn, &actor("josh", "codex", "one"), 1000, "exclusive");
    let recipient = BoardRecipient::parse("muse").unwrap();
    write(
        &mut conn,
        &actor("josh", "claude", "steward"),
        1050,
        |tx, ctx| move_task(tx, ctx, task, TaskColumn::Doing, Some(&recipient)),
    )
    .unwrap();
    let claims = read_claims(&conn, task.plan, 1050, 120).unwrap();
    assert_eq!(claims[0].ended_at, Some(1050));
    assert_eq!(claims[0].end_reason, Some(ClaimEndReason::Reassigned));
    assert_eq!(
        read_tasks(&conn, task.plan).unwrap()[0].assignee,
        Some(recipient)
    );
    assert!(claims.iter().all(|claim| claim.ended_at.is_some()));
}

#[test]
fn holders_card_moves_release_claim_into_every_non_working_column() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "codex", "one");
    for column in [
        TaskColumn::Review,
        TaskColumn::Done,
        TaskColumn::Blocked,
        TaskColumn::Todo,
    ] {
        let task = carved_task(&mut conn, &holder, 1000, "release");
        write(&mut conn, &holder, 1050, |tx, ctx| {
            move_task(tx, ctx, task, column, None)
        })
        .unwrap();
        let history = read_claims_window(&conn, task.plan, 1000, 1060, 1060, 120).unwrap();
        let claim = history.iter().find(|claim| claim.task == task).unwrap();
        assert_eq!(claim.end_reason, Some(ClaimEndReason::Released));
        assert_eq!(claim.ended_at, Some(1050));
    }
}

#[test]
fn historical_claims_intersect_window_with_original_scope_and_snapshot() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "codex", "one");
    let task = carved_task(&mut conn, &holder, 1000, "first scope");
    claim(&mut conn, &holder, 1050, task, "second scope").unwrap();
    let history = read_claims_window(&conn, task.plan, 1020, 1040, 1150, 120).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].scope.as_str(), "first scope");
    assert_eq!(history[0].model.as_deref(), Some("test-model"));
    assert_eq!(history[0].ended_at, Some(1050));
    assert!(
        read_claims_window(&conn, task.plan, 900, 999, 1150, 120)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn cached_claim_or_move_cannot_bypass_a_new_holder() {
    let database = ClaimDatabase::new();
    let mut backend =
        LocalBoard::open_path(&database.path, std::time::Duration::from_secs(120)).unwrap();
    let original = actor("josh", "codex", "one");
    let next = actor("josh", "muse", "two");
    let task = TaskId::new(PlanId::new(1).unwrap(), 1).unwrap();
    backend
        .handle(&BoardRequest::new(
            original.clone(),
            BoardOp::TaskCreate {
                plan: task.plan,
                title: PlanTitle::new("Retry safety").unwrap(),
                to: None,
                section: None,
            },
        ))
        .unwrap();
    let claim_request = BoardRequest::new(
        original.clone(),
        BoardOp::ClaimTask {
            task,
            scope: EntryText::new("my lane").unwrap(),
        },
    );
    backend.handle(&claim_request).unwrap();
    let release_request = BoardRequest::new(
        original,
        BoardOp::TaskMove {
            task,
            column: TaskColumn::Review,
            to: None,
        },
    );
    backend.handle(&release_request).unwrap();
    backend
        .handle(&BoardRequest::new(
            next.clone(),
            BoardOp::ClaimTask {
                task,
                scope: EntryText::new("new holder").unwrap(),
            },
        ))
        .unwrap();
    let claim_error = backend.handle(&claim_request).unwrap_err();
    assert!(claim_error.to_string().starts_with("claim_conflict:"));
    let move_error = backend.handle(&release_request).unwrap_err();
    assert!(move_error.to_string().starts_with("invalid_actor:"));
    let conn = database.connect();
    let history = read_claims(&conn, task.plan, i64::MAX, 120).unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|claim| claim.ended_at.is_none())
            .count(),
        1
    );
    assert_eq!(history.last().unwrap().actor, next);
}

#[test]
fn retried_carve_targets_original_task_after_handoff() {
    let database = ClaimDatabase::new();
    let mut backend =
        LocalBoard::open_path(&database.path, std::time::Duration::from_secs(120)).unwrap();
    let original = actor("josh", "codex", "one");
    let task = TaskId::new(PlanId::new(1).unwrap(), 1).unwrap();
    let request = BoardRequest::new(
        original.clone(),
        BoardOp::CarveClaim {
            plan: task.plan,
            title: PlanTitle::new("Atomic carve").unwrap(),
            scope: EntryText::new("my scope").unwrap(),
            section: Some("Claims".into()),
        },
    );
    backend.handle(&request).unwrap();
    backend
        .handle(&BoardRequest::new(
            original,
            BoardOp::TaskMove {
                task,
                column: TaskColumn::Todo,
                to: None,
            },
        ))
        .unwrap();
    backend
        .handle(&BoardRequest::new(
            actor("josh", "muse", "two"),
            BoardOp::ClaimTask {
                task,
                scope: EntryText::new("next scope").unwrap(),
            },
        ))
        .unwrap();
    assert!(
        backend
            .handle(&request)
            .unwrap_err()
            .to_string()
            .starts_with("claim_conflict:")
    );
    assert_eq!(read_tasks(&database.connect(), task.plan).unwrap().len(), 1);
}
