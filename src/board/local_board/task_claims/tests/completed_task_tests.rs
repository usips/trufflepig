use super::*;

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
        read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, 1051, 120)
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
            read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, 1053, 120)
                .unwrap()
                .iter()
                .find(|claim| claim.task == task && claim.ended_at.is_none())
                .unwrap()
                .actor,
            next
        );
    }
}
