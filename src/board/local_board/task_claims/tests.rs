mod claim_resumption;

use std::path::PathBuf;
use std::sync::{Arc, Barrier};

use super::*;
use crate::board::board_actor::{BoardActor, HarnessLabel};
use crate::board::board_backend::BoardBackend;
use crate::board::board_protocol::{BoardOp, BoardRequest};
use crate::board::board_vocabulary::TaskColumn;
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
        via: None,
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
        claim_task(tx, ctx, task, Some(&EntryText::new(scope).unwrap()), false)
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
        1050
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
        1050
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
            scope: Some(EntryText::new("my lane").unwrap()),
            resume: false,
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
                scope: Some(EntryText::new("new holder").unwrap()),
                resume: false,
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
fn retried_carve_creates_fresh_task_after_handoff() {
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
                scope: Some(EntryText::new("next scope").unwrap()),
                resume: false,
            },
        ))
        .unwrap();
    let reply = backend.handle(&request).unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("expected change")
    };
    assert_eq!(change.task.unwrap().ordinal, 2);
    assert!(!change.deduplicated);
    assert_eq!(read_tasks(&database.connect(), task.plan).unwrap().len(), 2);
    let claims = read_claims(&database.connect(), task.plan, i64::MAX, 120).unwrap();
    assert_eq!(
        claims
            .iter()
            .filter(|claim| claim.ended_at.is_none())
            .count(),
        2
    );
    assert_eq!(
        claims
            .iter()
            .find(|claim| claim.task == task && claim.ended_at.is_none())
            .unwrap()
            .actor
            .harness
            .as_str(),
        "muse"
    );
}

#[test]
fn delayed_commit_activity_keeps_silent_claim_stale() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "codex", "one");
    let task = carved_task(&mut conn, &holder, 1000, "current");
    let coauthor = CommitCoauthor {
        harness: HarnessLabel::parse("codex").unwrap(),
        model: "Codex".into(),
        email: "agent@openai.com".into(),
    };
    for committed_at in [1050, 1020] {
        write(&mut conn, &holder, 2000, |tx, ctx| {
            refresh_commit_claims(
                tx,
                task.plan,
                task.ordinal,
                &[coauthor.clone()],
                committed_at,
                ctx.now,
            )?;
            change(ctx, EntryId::new(1).unwrap(), task)
        })
        .unwrap();
    }
    let claim = &read_claims(&conn, task.plan, 2000, 120).unwrap()[0];
    assert_eq!(claim.last_active, 1050);
    assert!(
        claim.stale,
        "late ingestion must not create current activity"
    );
}

#[test]
fn commit_activity_uses_claimed_model_vendor_and_harness_fallback() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    for (index, (harness, model, vendor)) in [
        ("muse", Some("Claude Sonnet 4.5"), Some("claude")),
        ("omp", Some("gpt-6.1-sol"), Some("codex")),
        ("muse", Some("Codex"), Some("codex")),
        ("muse", Some("Kimi K2"), Some("kimi")),
        ("muse", Some("Grok 4"), Some("grok")),
        ("omp", Some("Gemini 3 Pro"), Some("gemini")),
        ("omp", Some("Qwen3"), Some("qwen")),
        ("codex", Some("unrecognized"), Some("codex")),
        ("claude", None, Some("claude")),
        ("cli", None, None),
        ("cli", Some("gpt-6.1-sol"), None),
        ("human", Some("Claude Sonnet 4.5"), None),
    ]
    .into_iter()
    .enumerate()
    {
        let holder = actor("josh", harness, &format!("vendor-{index}"));
        let task = carved_task(&mut conn, &holder, 1000, "vendor lane");
        conn.execute("UPDATE entries SET model=?1 WHERE id=(SELECT entry_id FROM claims WHERE plan_id=?2 AND task_ordinal=?3)", params![model, sql_number(task.plan.get()), sql_number(task.ordinal)]).unwrap();
        conn.execute(
            "UPDATE agent_sessions SET model='wrong current session model'",
            [],
        )
        .unwrap();
        let coauthors = vendor
            .map(|vendor| {
                vec![CommitCoauthor {
                    harness: HarnessLabel::parse(vendor).unwrap(),
                    model: "trailer model".into(),
                    email: "agent@example.invalid".into(),
                }]
            })
            .unwrap_or_default();
        write(&mut conn, &holder, 1100, |tx, ctx| {
            refresh_commit_claims(tx, task.plan, task.ordinal, &coauthors, 1050, ctx.now)?;
            change(ctx, EntryId::new(1).unwrap(), task)
        })
        .unwrap();
        let history = read_claims(&conn, task.plan, 1100, 120).unwrap();
        assert_eq!(
            history
                .iter()
                .find(|claim| claim.task == task)
                .unwrap()
                .last_active,
            1050,
            "{harness} / {model:?}"
        );
    }
}

#[test]
fn carve_retry_deduplicates_current_lease_but_creates_fresh_after_done() {
    let database = ClaimDatabase::new();
    let mut backend =
        LocalBoard::open_path(&database.path, std::time::Duration::from_secs(120)).unwrap();
    let holder = actor("josh", "codex", "one");
    let request = BoardRequest::new(
        holder.clone(),
        BoardOp::CarveClaim {
            plan: PlanId::new(1).unwrap(),
            title: PlanTitle::new("Carve retry").unwrap(),
            scope: EntryText::new("scope").unwrap(),
            section: None,
        },
    );
    let first = backend.handle(&request).unwrap();
    let BoardResult::Change(first) = first.result else {
        panic!("expected change")
    };
    let replay = backend.handle(&request).unwrap();
    let BoardResult::Change(replay) = replay.result else {
        panic!("expected change")
    };
    assert!(replay.deduplicated);
    assert_eq!(first.task, replay.task);
    backend
        .handle(&BoardRequest::new(
            holder,
            BoardOp::TaskMove {
                task: first.task.unwrap(),
                column: TaskColumn::Done,
                to: None,
            },
        ))
        .unwrap();
    let retried = backend.handle(&request).unwrap();
    let BoardResult::Change(retried) = retried.result else {
        panic!("expected change")
    };
    assert_ne!(first.task, retried.task);
    assert!(!retried.deduplicated);
    let tasks = read_tasks(&database.connect(), first.plan.unwrap()).unwrap();
    assert_eq!(tasks[0].column, TaskColumn::Done);
    assert_eq!(tasks[1].column, TaskColumn::Doing);
}

#[test]
fn doing_without_recipient_records_callers_exclusive_claim() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "muse", "one");
    let reply = write(&mut conn, &holder, 1000, |tx, ctx| {
        create_task(
            tx,
            ctx,
            PlanId::new(1).unwrap(),
            &PlanTitle::new("Unclaimed card").unwrap(),
            None,
            None,
        )
    })
    .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("expected change")
    };
    let task = change.task.unwrap();
    write(&mut conn, &holder, 1050, |tx, ctx| {
        move_task(tx, ctx, task, TaskColumn::Doing, None)
    })
    .unwrap();
    let claims = read_claims(&conn, task.plan, 1050, 120).unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].actor, holder);
    assert_eq!(claims[0].scope.as_str(), "Unclaimed card");
    assert_eq!(
        read_tasks(&conn, task.plan).unwrap()[0].assignee,
        Some(BoardRecipient::for_actor(&holder))
    );
    assert!(
        claim(
            &mut conn,
            &actor("josh", "codex", "other"),
            1051,
            task,
            "steal"
        )
        .is_err()
    );
}

#[test]
fn reassigned_card_reserves_claim_and_move_for_recipient() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let original = actor("josh", "codex", "one");
    let task = carved_task(&mut conn, &original, 1000, "exclusive");
    let recipient = BoardRecipient::parse("muse").unwrap();
    let steward = actor("josh", "claude", "steward");
    write(&mut conn, &steward, 1050, |tx, ctx| {
        move_task(tx, ctx, task, TaskColumn::Doing, Some(&recipient))
    })
    .unwrap();
    for privileged in [&steward, &actor("josh", "human", "owner")] {
        assert!(claim(&mut conn, privileged, 1051, task, "steal assignment").is_err());
    }
    for unauthorized in [
        original,
        actor("other", "human", "owner"),
        actor("other", "claude", "steward"),
    ] {
        assert!(claim(&mut conn, &unauthorized, 1051, task, "steal assignment").is_err());
        assert!(
            write(&mut conn, &unauthorized, 1051, |tx, ctx| move_task(
                tx,
                ctx,
                task,
                TaskColumn::Doing,
                Some(&recipient)
            ))
            .is_err()
        );
        for column in [TaskColumn::Doing, TaskColumn::Review, TaskColumn::Done] {
            let error = write(&mut conn, &unauthorized, 1051, |tx, ctx| {
                move_task(tx, ctx, task, column, None)
            })
            .unwrap_err();
            assert!(error.to_string().starts_with("invalid_actor:"), "{error}");
        }
    }
    let assignee = actor("josh", "muse", "recipient");
    claim(&mut conn, &assignee, 1052, task, "recipient scope").unwrap();
    let history = read_claims(&conn, task.plan, 1052, 120).unwrap();
    assert_eq!(history.last().unwrap().actor, assignee);
    assert!(history.last().unwrap().ended_at.is_none());
    write(&mut conn, &assignee, 1053, |tx, ctx| {
        move_task(tx, ctx, task, TaskColumn::Review, None)
    })
    .unwrap();
    assert_eq!(
        read_tasks(&conn, task.plan).unwrap()[0].column,
        TaskColumn::Review
    );
}

#[test]
fn completed_card_cannot_be_reclaimed_or_reassigned_to_doing() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "codex", "one");
    let task = carved_task(&mut conn, &holder, 1000, "complete");
    write(&mut conn, &holder, 1050, |tx, ctx| {
        move_task(tx, ctx, task, TaskColumn::Done, None)
    })
    .unwrap();
    for caller in [holder, actor("josh", "claude", "steward")] {
        let error = claim(&mut conn, &caller, 1051, task, "reopen").unwrap_err();
        assert!(error.to_string().starts_with("invalid_state:"), "{error}");
        for recipient in [None, Some(BoardRecipient::parse("muse").unwrap())] {
            let error = write(&mut conn, &caller, 1051, |tx, ctx| {
                move_task(tx, ctx, task, TaskColumn::Doing, recipient.as_ref())
            })
            .unwrap_err();
            assert!(error.to_string().starts_with("invalid_state:"), "{error}");
        }
    }
    assert_eq!(
        read_tasks(&conn, task.plan).unwrap()[0].column,
        TaskColumn::Done
    );
    assert!(
        read_claims(&conn, task.plan, 1051, 120)
            .unwrap()
            .iter()
            .all(|claim| claim.ended_at.is_some())
    );
}

#[test]
fn owner_authority_can_redirect_or_cancel_pending_assignment() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "codex", "one");
    let steward = actor("josh", "claude", "steward");
    for privileged in [&steward, &actor("josh", "human", "owner")] {
        let task = carved_task(&mut conn, &holder, 1000, "pending assignment");
        let recipient = BoardRecipient::parse("muse").unwrap();
        write(&mut conn, &steward, 1050, |tx, ctx| {
            move_task(tx, ctx, task, TaskColumn::Doing, Some(&recipient))
        })
        .unwrap();
        let redirect = BoardRecipient::parse("kimi").unwrap();
        write(&mut conn, privileged, 1051, |tx, ctx| {
            move_task(tx, ctx, task, TaskColumn::Doing, Some(&redirect))
        })
        .unwrap();
        let tasks = read_tasks(&conn, task.plan).unwrap();
        assert_eq!(
            tasks.iter().find(|card| card.id == task).unwrap().assignee,
            Some(redirect)
        );
        write(&mut conn, privileged, 1052, |tx, ctx| {
            move_task(tx, ctx, task, TaskColumn::Todo, None)
        })
        .unwrap();
        assert_eq!(
            read_tasks(&conn, task.plan)
                .unwrap()
                .iter()
                .find(|card| card.id == task)
                .unwrap()
                .column,
            TaskColumn::Todo
        );
        claim(&mut conn, &holder, 1053, task, "after cancellation").unwrap();
    }
}

#[test]
fn explicit_owner_correction_reopens_done_to_todo_before_normal_claim() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "codex", "one");
    for privileged in [
        actor("josh", "claude", "steward"),
        actor("josh", "human", "owner"),
    ] {
        let task = carved_task(&mut conn, &holder, 1000, "completed");
        write(&mut conn, &holder, 1050, |tx, ctx| {
            move_task(tx, ctx, task, TaskColumn::Done, None)
        })
        .unwrap();
        for unauthorized in [
            &holder,
            &actor("other", "human", "owner"),
            &actor("other", "claude", "steward"),
        ] {
            assert!(
                write(&mut conn, unauthorized, 1051, |tx, ctx| move_task(
                    tx,
                    ctx,
                    task,
                    TaskColumn::Todo,
                    None
                ))
                .is_err()
            );
        }
        assert!(claim(&mut conn, &privileged, 1051, task, "direct reclaim").is_err());
        write(&mut conn, &privileged, 1052, |tx, ctx| {
            move_task(tx, ctx, task, TaskColumn::Todo, None)
        })
        .unwrap();
        let next = actor("josh", "muse", "after-correction");
        claim(&mut conn, &next, 1053, task, "corrected scope").unwrap();
        assert_eq!(
            read_claims(&conn, task.plan, 1053, 120)
                .unwrap()
                .iter()
                .find(|claim| claim.task == task && claim.ended_at.is_none())
                .unwrap()
                .actor,
            next
        );
    }
}
