use super::*;

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
