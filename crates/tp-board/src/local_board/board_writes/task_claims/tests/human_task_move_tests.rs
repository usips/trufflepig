use super::*;
use crate::board_protocol::{WorkerHandle, WorkerRole};

fn create_task_for_owner(
    conn: &mut Connection,
    owner: &BoardActor,
    now: i64,
    title: &str,
    assignee: Option<&BoardRecipient>,
) -> TaskId {
    let title = PlanTitle::new(title).unwrap();
    let reply = write(conn, owner, now, |tx, ctx| {
        create_task(tx, ctx, PlanId::new(1).unwrap(), &title, assignee, None)
    })
    .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("expected task creation change");
    };
    change.task.unwrap()
}

fn change_entry(reply: BoardReply, task: TaskId) -> EntryId {
    let BoardResult::Change(change) = reply.result else {
        panic!("expected task change");
    };
    assert_eq!(change.task, Some(task));
    change.entry
}

fn assert_owner_audit(conn: &Connection, entry: EntryId) {
    let entry_id = i64::try_from(entry.get()).unwrap();
    let entry_authority: (String, Option<i64>) = conn
        .query_row(
            "SELECT authority_basis,authority_entry FROM entries WHERE id=?1",
            [entry_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(entry_authority, ("owner".to_owned(), None));

    let event_authority: (String, Option<i64>) = conn
        .query_row(
            "SELECT authority_basis,authority_entry FROM events WHERE seq=(SELECT seq FROM entries WHERE id=?1)",
            [entry_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(event_authority, ("owner".to_owned(), None));
}

fn add_test_worker(conn: &mut Connection, holder: &BoardActor, now: i64, task: TaskId) {
    let handle = WorkerHandle::parse("owner-move-worker").unwrap();
    write(conn, holder, now, |tx, ctx| {
        add_worker(
            tx,
            ctx,
            task,
            &handle,
            WorkerRole::Code,
            "worker-model",
            Some("high"),
            &EntryText::new("owner move release coverage").unwrap(),
            &["src/owner_move.rs".to_owned()],
        )
    })
    .unwrap();
}

#[test]
fn human_owner_moves_unclaimed_review_task_to_done_through_board_handle() {
    let database = ClaimDatabase::new();
    let mut board =
        LocalBoard::open_path(&database.path, std::time::Duration::from_secs(120)).unwrap();
    let owner = actor("josh", "human", "owner");
    let BoardResult::Change(created) = board
        .handle(&BoardRequest::new(
            owner.clone(),
            BoardOp::TaskCreate {
                plan: PlanId::new(1).unwrap(),
                title: PlanTitle::new("Unclaimed owner review").unwrap(),
                to: None,
                section: None,
            },
        ))
        .unwrap()
        .result
    else {
        panic!("expected task creation change");
    };
    let task = created.task.unwrap();

    let review = board
        .handle(&BoardRequest::new(
            owner.clone(),
            BoardOp::TaskMove {
                task,
                column: TaskColumn::Review,
                to: None,
            },
        ))
        .unwrap();
    let conn = database.connect();
    assert_owner_audit(&conn, change_entry(review, task));
    assert_eq!(
        read_tasks(&conn, task.plan).unwrap()[0].column,
        TaskColumn::Review
    );
    assert!(
        read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, i64::MAX, 120)
            .unwrap()
            .iter()
            .all(|claim| claim.task != task)
    );

    let done = board
        .handle(&BoardRequest::new(
            owner,
            BoardOp::TaskMove {
                task,
                column: TaskColumn::Done,
                to: None,
            },
        ))
        .unwrap();
    assert_owner_audit(&conn, change_entry(done, task));
    assert_eq!(
        read_tasks(&conn, task.plan).unwrap()[0].column,
        TaskColumn::Done
    );
    assert!(
        read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, i64::MAX, 120)
            .unwrap()
            .iter()
            .all(|claim| claim.task != task)
    );
}

#[test]
fn human_owner_moves_review_and_done_over_active_and_stale_claims() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let owner = actor("josh", "human", "owner");
    let holder = actor("agent", "muse", "holder");

    for (column_index, column) in [TaskColumn::Review, TaskColumn::Done]
        .into_iter()
        .enumerate()
    {
        for stale in [false, true] {
            let stale_offset = if stale { 1000 } else { 0 };
            let start = 1000 + i64::try_from(column_index * 2).unwrap() * 1000 + stale_offset;
            let assignee = BoardRecipient::parse("muse").unwrap();
            let task = create_task_for_owner(
                &mut conn,
                &owner,
                start,
                "Claimed owner move",
                Some(&assignee),
            );
            claim(&mut conn, &holder, start, task, "holder scope").unwrap();
            add_test_worker(&mut conn, &holder, start + 1, task);

            let now = start + if stale { 122 } else { 50 };
            let claims =
                read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, now, 120).unwrap();
            let current = claims.iter().find(|claim| claim.task == task).unwrap();
            assert_eq!(current.stale, stale);
            assert_eq!(current.ended_at, None);

            let reply = write(&mut conn, &owner, now, |tx, ctx| {
                move_task(tx, ctx, task, column, None)
            })
            .unwrap();
            assert_owner_audit(&conn, change_entry(reply, task));

            let card = read_tasks(&conn, task.plan)
                .unwrap()
                .into_iter()
                .find(|card| card.id == task)
                .unwrap();
            assert_eq!(card.column, column);
            assert_eq!(card.assignee, Some(BoardRecipient::for_actor(&holder)));

            let ended_claim = read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, now, 120)
                .unwrap()
                .into_iter()
                .find(|claim| claim.task == task)
                .unwrap();
            assert_eq!(ended_claim.ended_at, Some(now));
            assert_eq!(ended_claim.end_reason, Some(ClaimEndReason::Released));
            let worker_end: Option<i64> = conn
                .query_row(
                    "SELECT ended_at FROM claim_workers WHERE plan_id=?1 AND task_ordinal=?2",
                    rusqlite::params![sql_number(task.plan.get()), sql_number(task.ordinal)],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(worker_end, Some(now));
        }
    }
}

#[test]
fn agents_cli_and_foreign_human_cannot_review_or_complete_without_claim() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    grant_legacy_claude(&conn);
    let task = create_task_for_owner(
        &mut conn,
        &actor("josh", "human", "owner"),
        1000,
        "Unclaimed protected task",
        None,
    );
    let actors = [
        actor("josh", "codex", "owner-user-agent"),
        actor("josh", "cli", "owner-user-cli"),
        actor("other", "human", "foreign-human"),
        actor("josh", "claude", "all-role-grantee"),
    ];

    for column in [TaskColumn::Review, TaskColumn::Done] {
        for caller in &actors {
            let error = write(&mut conn, caller, 1010, |tx, ctx| {
                move_task(tx, ctx, task, column, None)
            })
            .unwrap_err()
            .to_string();
            assert!(error.contains("only the active claim holder"), "{error}");
        }
    }

    conn.execute(
        "UPDATE authority_grants SET roles_json='[\"decide\"]' WHERE grantee='claude'",
        [],
    )
    .unwrap();
    let decide_grantee = actor("josh", "claude", "decide-only-grantee");
    for column in [TaskColumn::Review, TaskColumn::Done] {
        let error = write(&mut conn, &decide_grantee, 1010, |tx, ctx| {
            move_task(tx, ctx, task, column, None)
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("only the active claim holder"), "{error}");
    }

    assert_eq!(
        read_tasks(&conn, task.plan).unwrap()[0].column,
        TaskColumn::Todo
    );
    assert!(
        read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, 1010, 120)
            .unwrap()
            .iter()
            .all(|claim| claim.task != task)
    );
}
