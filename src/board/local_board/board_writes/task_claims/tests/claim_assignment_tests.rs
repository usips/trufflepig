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
        read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, 1050, 120).unwrap()[0]
            .ended_at
            .is_none()
    );
}

#[test]
fn task_creator_and_assignee_has_no_admin_authority_without_a_grant() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let creator = actor("josh", "codex", "creator");
    let creator_recipient = BoardRecipient::for_actor(&creator);
    let created = write(&mut conn, &creator, 1000, |tx, ctx| {
        create_task(
            tx,
            ctx,
            PlanId::new(1).unwrap(),
            &PlanTitle::new("unassigned card").unwrap(),
            Some(&creator_recipient),
            None,
        )
    })
    .unwrap();
    let BoardResult::Change(change) = created.result else {
        panic!("expected task creation change")
    };
    let task = change.task.unwrap();
    let recipient = BoardRecipient::parse("muse").unwrap();
    let error = write(&mut conn, &creator, 1010, |tx, ctx| {
        move_task(tx, ctx, task, TaskColumn::Doing, Some(&recipient))
    })
    .unwrap_err();
    assert!(error.to_string().starts_with("invalid_actor:"), "{error}");
}

#[test]
fn task_creator_can_move_own_unassigned_unclaimed_card_between_todo_and_blocked() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let creator = actor("josh", "codex", "creator");
    let task = create_unassigned_task(&mut conn, &creator, 1000, "creator-owned card");

    let before = persisted_counts(&conn);
    let same_column = write(&mut conn, &creator, 1005, |tx, ctx| {
        move_task(tx, ctx, task, TaskColumn::Todo, None)
    })
    .unwrap_err();
    assert!(same_column.to_string().starts_with("invalid_actor:"));
    assert_eq!(persisted_counts(&conn), before);

    for (column, now) in [(TaskColumn::Blocked, 1010), (TaskColumn::Todo, 1020)] {
        let reply = write(&mut conn, &creator, now, |tx, ctx| {
            move_task(tx, ctx, task, column, None)
        })
        .unwrap();
        let BoardResult::Change(change) = reply.result else {
            panic!("expected task move change")
        };
        assert_eq!(read_tasks(&conn, task.plan).unwrap()[0].column, column);
        assert_creator_authority(&conn, task, change.entry);
    }
}

#[test]
fn creator_move_authority_does_not_bypass_assignees_or_active_claims() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let creator = actor("josh", "codex", "creator");
    let stranger = actor("other", "muse", "stranger");
    let assignee = actor("josh", "claude", "assignee");
    let owner = actor("josh", "human", "owner");

    let unassigned = create_unassigned_task(&mut conn, &creator, 1000, "unassigned card");
    let before = persisted_counts(&conn);
    let error = write(&mut conn, &stranger, 1010, |tx, ctx| {
        move_task(tx, ctx, unassigned, TaskColumn::Blocked, None)
    })
    .unwrap_err();
    assert!(error.to_string().starts_with("invalid_actor:"), "{error}");
    assert_eq!(persisted_counts(&conn), before);

    let recipient = BoardRecipient::parse("muse").unwrap();
    for (column, to, now) in [
        (TaskColumn::Doing, Some(&recipient), 1016),
        (TaskColumn::Review, None, 1017),
        (TaskColumn::Done, None, 1018),
    ] {
        let before = persisted_counts(&conn);
        let error = write(&mut conn, &creator, now, |tx, ctx| {
            move_task(tx, ctx, unassigned, column, to)
        })
        .unwrap_err();
        assert!(error.to_string().starts_with("invalid_actor:"), "{error}");
        assert_eq!(persisted_counts(&conn), before);
    }

    let assign_grant = create_assign_grant(
        &mut conn,
        &owner,
        &creator,
        PlanId::new(1).unwrap(),
        1019,
        None,
    );
    let assigned = create_task_for_actor(&mut conn, &creator, &assignee, 1020, "assigned card");
    write(&mut conn, &owner, 1025, |tx, ctx| {
        crate::board::local_board::board_authority::write_revoke(
            tx,
            ctx,
            &BoardOp::Revoke {
                entry: assign_grant,
            },
        )
    })
    .unwrap();
    let before = persisted_counts(&conn);
    let error = write(&mut conn, &creator, 1030, |tx, ctx| {
        move_task(tx, ctx, assigned, TaskColumn::Blocked, None)
    })
    .unwrap_err();
    assert!(error.to_string().starts_with("invalid_actor:"), "{error}");
    assert_eq!(persisted_counts(&conn), before);

    let claimed = create_unassigned_task(&mut conn, &creator, 1040, "claimed card");
    claim(&mut conn, &stranger, 1050, claimed, "active holder").unwrap();
    let before = persisted_counts(&conn);
    let error = write(&mut conn, &creator, 1060, |tx, ctx| {
        move_task(tx, ctx, claimed, TaskColumn::Blocked, None)
    })
    .unwrap_err();
    assert!(error.to_string().starts_with("invalid_actor:"), "{error}");
    assert_eq!(persisted_counts(&conn), before);
    let claimed_card = read_tasks(&conn, claimed.plan)
        .unwrap()
        .into_iter()
        .find(|card| card.id == claimed)
        .unwrap();
    assert_eq!(claimed_card.column, TaskColumn::Doing);
}

#[test]
fn assign_grant_reassignment_ends_claim_and_assigns_recipient() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    grant_legacy_claude(&conn);
    let task = carved_task(&mut conn, &actor("josh", "codex", "one"), 1000, "exclusive");
    let recipient = BoardRecipient::parse("muse").unwrap();
    write(
        &mut conn,
        &actor("josh", "claude", "grant"),
        1050,
        |tx, ctx| move_task(tx, ctx, task, TaskColumn::Doing, Some(&recipient)),
    )
    .unwrap();
    let claims = read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, 1050, 120).unwrap();
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
    let claims = read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, 1050, 120).unwrap();
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
    grant_legacy_claude(&conn);
    let original = actor("josh", "codex", "one");
    let task = carved_task(&mut conn, &original, 1000, "exclusive");
    let recipient = BoardRecipient::parse("muse").unwrap();
    let grant_holder = actor("josh", "claude", "grant");
    write(&mut conn, &grant_holder, 1050, |tx, ctx| {
        move_task(tx, ctx, task, TaskColumn::Doing, Some(&recipient))
    })
    .unwrap();
    for privileged in [&grant_holder, &actor("josh", "human", "owner")] {
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
    let history = read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, 1052, 120).unwrap();
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
fn assign_grant_moves_unclaimed_review_card_without_changing_assignee() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let assignee = actor("josh", "codex", "assignee");
    let tasks = [
        unclaimed_review_task(&mut conn, &assignee, "review to blocked"),
        unclaimed_review_task(&mut conn, &assignee, "review to todo"),
    ];
    let assigner = actor("josh", "claude", "assign-only");
    let grant = grant_role_to_actor(&conn, &assigner, "assign", 1015);
    assert_grant_role(&conn, grant, "assign");

    for (task, column, now) in [
        (tasks[0], TaskColumn::Blocked, 1020),
        (tasks[1], TaskColumn::Todo, 1021),
    ] {
        let reply = write(&mut conn, &assigner, now, |tx, ctx| {
            move_task(tx, ctx, task, column, None)
        })
        .unwrap();
        let BoardResult::Change(change) = reply.result else {
            panic!("expected task move change")
        };
        let card = read_tasks(&conn, task.plan)
            .unwrap()
            .into_iter()
            .find(|card| card.id == task)
            .unwrap();
        assert_eq!(card.column, column);
        assert_eq!(card.assignee, Some(BoardRecipient::for_actor(&assignee)));
        let claims = read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, now, 120).unwrap();
        let task_claims: Vec<_> = claims.iter().filter(|claim| claim.task == task).collect();
        assert_eq!(task_claims.len(), 1);
        assert_eq!(task_claims[0].ended_at, Some(1010));
        assert_eq!(task_claims[0].end_reason, Some(ClaimEndReason::Released));
        assert_grant_authority(&conn, task, change.entry, grant);
    }
}

#[test]
fn plain_release_move_without_grant_names_assign_and_to_without_mutation() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let assignee = actor("josh", "codex", "assignee");
    let task = unclaimed_review_task(&mut conn, &assignee, "review without grant");
    let before = persisted_counts(&conn);
    let error = write(
        &mut conn,
        &actor("other", "muse", "no-grant"),
        1020,
        |tx, ctx| move_task(tx, ctx, task, TaskColumn::Blocked, None),
    )
    .unwrap_err();
    assert!(error.to_string().contains("assign authority"), "{error}");
    assert!(error.to_string().contains("--to"), "{error}");
    assert_eq!(persisted_counts(&conn), before);
    assert_unclaimed_review_state(&conn, task, &assignee);
}

#[test]
fn plain_release_moves_reject_revoked_expired_and_wrong_plan_assign_grants() {
    let assignee = actor("josh", "codex", "assignee");
    let assigner = actor("josh", "claude", "assign-only");
    let grantor = actor("josh", "human", "grant-owner");
    let plan = PlanId::new(1).unwrap();

    {
        let database = ClaimDatabase::new();
        let mut conn = database.connect();
        let tasks = [
            unclaimed_review_task(&mut conn, &assignee, "revoked grant to todo"),
            unclaimed_review_task(&mut conn, &assignee, "revoked grant to blocked"),
        ];
        let grant = create_assign_grant(&mut conn, &grantor, &assigner, plan, 1015, None);
        assert_exact_assign_grant(&conn, grant, plan, &assigner, None, None);
        let reply = write(&mut conn, &grantor, 1016, |tx, ctx| {
            crate::board::local_board::board_authority::write_revoke(
                tx,
                ctx,
                &crate::board::board_protocol::BoardOp::Revoke {
                    entry: grant.clone(),
                },
            )
        })
        .unwrap();
        let BoardResult::Change(change) = reply.result else {
            panic!("expected grant revocation change")
        };
        assert_exact_assign_grant(&conn, grant, plan, &assigner, None, Some(change.entry));
        assert_invalid_assign_cannot_plain_release(&mut conn, &assigner, &assignee, tasks, 1020);
    }

    {
        let database = ClaimDatabase::new();
        let mut conn = database.connect();
        let tasks = [
            unclaimed_review_task(&mut conn, &assignee, "expired grant to todo"),
            unclaimed_review_task(&mut conn, &assignee, "expired grant to blocked"),
        ];
        let boundary = unclaimed_review_task(&mut conn, &assignee, "expired grant boundary");
        let grant = create_assign_grant(&mut conn, &grantor, &assigner, plan, 1015, Some(5));
        assert_exact_assign_grant(&conn, grant, plan, &assigner, Some(1020), None);
        // expires_at is exclusive: the grant authorizes a plain release at 1019 but not 1020.
        write(&mut conn, &assigner, 1019, |tx, ctx| {
            move_task(tx, ctx, boundary, TaskColumn::Todo, None)
        })
        .unwrap();
        let card = read_tasks(&conn, plan)
            .unwrap()
            .into_iter()
            .find(|card| card.id == boundary)
            .unwrap();
        assert_eq!(card.column, TaskColumn::Todo);
        assert_invalid_assign_cannot_plain_release(&mut conn, &assigner, &assignee, tasks, 1020);
    }

    {
        let database = ClaimDatabase::new();
        let mut conn = database.connect();
        conn.execute(
            "INSERT INTO plans(id,title,owner_user,head_revision,next_task,created_at) VALUES(2,'Other plan','josh',1,0,1000)",
            [],
        )
        .unwrap();
        let tasks = [
            unclaimed_review_task(&mut conn, &assignee, "other plan grant to todo"),
            unclaimed_review_task(&mut conn, &assignee, "other plan grant to blocked"),
        ];
        let other_plan = PlanId::new(2).unwrap();
        let grant = create_assign_grant(&mut conn, &grantor, &assigner, other_plan, 1015, None);
        assert_exact_assign_grant(&conn, grant, other_plan, &assigner, None, None);
        assert_invalid_assign_cannot_plain_release(&mut conn, &assigner, &assignee, tasks, 1020);
    }
}

#[test]
fn assign_grant_does_not_authorize_review_or_done_without_a_claim() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let assignee = actor("josh", "codex", "assignee");
    let task = unclaimed_review_task(&mut conn, &assignee, "review needs a holder");
    let assigner = actor("josh", "claude", "assign-only");
    let grant = grant_role_to_actor(&conn, &assigner, "assign", 1015);
    assert_grant_role(&conn, grant, "assign");

    for column in [TaskColumn::Review, TaskColumn::Done] {
        let before = persisted_counts(&conn);
        let error = write(&mut conn, &assigner, 1020, |tx, ctx| {
            move_task(tx, ctx, task, column, None)
        })
        .unwrap_err();
        assert!(
            error.to_string().contains("only the active claim holder"),
            "{error}"
        );
        assert_eq!(persisted_counts(&conn), before);
        assert_unclaimed_review_state(&conn, task, &assignee);
    }

    let holder = actor("josh", "codex", "done-holder");
    let done_task = carved_task(&mut conn, &holder, 1000, "done needs decide");
    write(&mut conn, &holder, 1010, |tx, ctx| {
        move_task(tx, ctx, done_task, TaskColumn::Done, None)
    })
    .unwrap();
    let before = persisted_counts(&conn);
    let error = write(&mut conn, &assigner, 1020, |tx, ctx| {
        move_task(tx, ctx, done_task, TaskColumn::Todo, None)
    })
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("can only be reopened to todo with decide authority"),
        "{error}"
    );
    assert_eq!(persisted_counts(&conn), before);
    let card = read_tasks(&conn, done_task.plan)
        .unwrap()
        .into_iter()
        .find(|card| card.id == done_task)
        .unwrap();
    assert_eq!(card.column, TaskColumn::Done);
    let claims = read_claims_window(&conn, done_task.plan, i64::MIN, i64::MAX, 1020, 120).unwrap();
    let task_claims: Vec<_> = claims
        .iter()
        .filter(|claim| claim.task == done_task)
        .collect();
    assert_eq!(task_claims.len(), 1);
    assert_eq!(task_claims[0].ended_at, Some(1010));
}

#[test]
fn assign_grant_cannot_release_another_actors_active_claim() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "codex", "holder");
    let task = carved_task(&mut conn, &holder, 1000, "held card");
    let assigner = actor("josh", "claude", "assign-only");
    let grant = grant_role_to_actor(&conn, &assigner, "assign", 1015);
    assert_grant_role(&conn, grant, "assign");
    let before = persisted_counts(&conn);

    let error = write(&mut conn, &assigner, 1020, |tx, ctx| {
        move_task(tx, ctx, task, TaskColumn::Blocked, None)
    })
    .unwrap_err();
    assert!(error.to_string().contains("cannot move"), "{error}");
    assert_eq!(persisted_counts(&conn), before);
    let card = read_tasks(&conn, task.plan).unwrap()[0].clone();
    assert_eq!(card.column, TaskColumn::Doing);
    assert_eq!(card.assignee, Some(BoardRecipient::for_actor(&holder)));
    let claims = read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, 1020, 120).unwrap();
    let task_claims: Vec<_> = claims.iter().filter(|claim| claim.task == task).collect();
    assert_eq!(task_claims.len(), 1);
    assert_eq!(task_claims[0].ended_at, None);
}

#[test]
fn assign_grantee_cannot_move_pending_doing_card_away_from_assignee() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "codex", "holder");
    let task = carved_task(&mut conn, &holder, 1000, "pending Doing card");
    let assigner = actor("josh", "claude", "assign-only");
    let grant = grant_role_to_actor(&conn, &assigner, "assign", 1015);
    assert_grant_role(&conn, grant, "assign");
    let recipient = BoardRecipient::parse("muse").unwrap();
    write(&mut conn, &assigner, 1020, |tx, ctx| {
        move_task(tx, ctx, task, TaskColumn::Doing, Some(&recipient))
    })
    .unwrap();
    let before = persisted_counts(&conn);

    let error = write(&mut conn, &assigner, 1030, |tx, ctx| {
        move_task(tx, ctx, task, TaskColumn::Doing, None)
    })
    .unwrap_err();
    assert!(
        error.to_string().contains("reserved for its assignee"),
        "{error}"
    );
    assert_eq!(persisted_counts(&conn), before);
    let card = read_tasks(&conn, task.plan)
        .unwrap()
        .into_iter()
        .find(|card| card.id == task)
        .unwrap();
    assert_eq!(card.column, TaskColumn::Doing);
    assert_eq!(card.assignee, Some(recipient));
    let claims = read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, 1030, 120).unwrap();
    let task_claims: Vec<_> = claims.iter().filter(|claim| claim.task == task).collect();
    assert_eq!(task_claims.len(), 1);
    assert_eq!(task_claims[0].end_reason, Some(ClaimEndReason::Reassigned));
    assert!(task_claims[0].ended_at.is_some());
}

#[test]
fn decide_only_grant_reopens_done_without_assign_authority() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "codex", "holder");
    let task = carved_task(&mut conn, &holder, 1000, "completed card");
    write(&mut conn, &holder, 1010, |tx, ctx| {
        move_task(tx, ctx, task, TaskColumn::Done, None)
    })
    .unwrap();
    let decider = actor("josh", "claude", "decide-only");
    let grant = grant_role_to_actor(&conn, &decider, "decide", 1015);
    assert_grant_role(&conn, grant, "decide");

    let reply = write(&mut conn, &decider, 1020, |tx, ctx| {
        move_task(tx, ctx, task, TaskColumn::Todo, None)
    })
    .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("expected task move change")
    };
    assert_eq!(
        read_tasks(&conn, task.plan).unwrap()[0].column,
        TaskColumn::Todo
    );
    assert_grant_authority(&conn, task, change.entry, grant);
}

fn unclaimed_review_task(conn: &mut Connection, assignee: &BoardActor, scope: &str) -> TaskId {
    let task = carved_task(conn, assignee, 1000, scope);
    write(conn, assignee, 1010, |tx, ctx| {
        move_task(tx, ctx, task, TaskColumn::Review, None)
    })
    .unwrap();
    task
}

fn create_assign_grant(
    conn: &mut Connection,
    grantor: &BoardActor,
    assigner: &BoardActor,
    plan: PlanId,
    now: i64,
    expires_in: Option<u64>,
) -> crate::board::board_ids::EntryId {
    let reply = write(conn, grantor, now, |tx, ctx| {
        crate::board::local_board::board_authority::write_grant(
            tx,
            ctx,
            &crate::board::board_protocol::BoardOp::Grant {
                plan: Some(plan),
                roles: vec![crate::board::board_protocol::AuthorityRole::Assign],
                grantee: BoardRecipient::for_actor(assigner),
                expires_in,
                note: None,
            },
        )
    })
    .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("expected grant change")
    };
    change.entry
}

fn assert_exact_assign_grant(
    conn: &Connection,
    grant: crate::board::board_ids::EntryId,
    plan: PlanId,
    assigner: &BoardActor,
    expires_at: Option<i64>,
    revoked_by: Option<crate::board::board_ids::EntryId>,
) {
    let grant_id = i64::try_from(grant.get()).unwrap();
    assert_grant_role(conn, grant_id, "assign");
    let actual = conn
        .query_row(
            "SELECT plan_id,grantee,expires_at,revoked_by_entry FROM authority_grants WHERE entry_id=?1",
            [grant_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(actual.0, i64::try_from(plan.get()).unwrap());
    assert_eq!(actual.1, BoardRecipient::for_actor(assigner).as_str());
    assert_eq!(actual.2, expires_at);
    assert_eq!(
        actual.3,
        revoked_by.map(|entry| i64::try_from(entry.get()).unwrap())
    );
}

fn assert_invalid_assign_cannot_plain_release(
    conn: &mut Connection,
    assigner: &BoardActor,
    assignee: &BoardActor,
    tasks: [TaskId; 2],
    now: i64,
) {
    for (task, column) in [
        (tasks[0], TaskColumn::Todo),
        (tasks[1], TaskColumn::Blocked),
    ] {
        let before = persisted_counts(conn);
        let error = write(conn, assigner, now, |tx, ctx| {
            move_task(tx, ctx, task, column, None)
        })
        .unwrap_err();
        assert!(error.to_string().contains("assign authority"), "{error}");
        assert_eq!(
            persisted_counts(conn),
            before,
            "denied {column} move added an entry or event"
        );
        assert_unclaimed_review_state(conn, task, assignee);
    }
}

fn assert_unclaimed_review_state(conn: &Connection, task: TaskId, assignee: &BoardActor) {
    let card = read_tasks(conn, task.plan)
        .unwrap()
        .into_iter()
        .find(|card| card.id == task)
        .unwrap();
    assert_eq!(card.column, TaskColumn::Review);
    assert_eq!(card.assignee, Some(BoardRecipient::for_actor(assignee)));
    let claims = read_claims_window(conn, task.plan, i64::MIN, i64::MAX, 1020, 120).unwrap();
    let task_claims: Vec<_> = claims.iter().filter(|claim| claim.task == task).collect();
    assert_eq!(task_claims.len(), 1);
    assert_eq!(task_claims[0].ended_at, Some(1010));
    assert_eq!(task_claims[0].end_reason, Some(ClaimEndReason::Released));
}

fn persisted_counts(conn: &Connection) -> (i64, i64, i64, i64) {
    let entries = conn
        .query_row(
            "SELECT COUNT(*),COALESCE(MAX(seq),0) FROM entries",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let events = conn
        .query_row(
            "SELECT COUNT(*),COALESCE(MAX(seq),0) FROM events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    (entries.0, entries.1, events.0, events.1)
}

fn grant_role_to_actor(conn: &Connection, grantee: &BoardActor, role: &str, now: i64) -> i64 {
    let grantor = actor("josh", "human", "grant-owner");
    conn.execute(
        "INSERT OR IGNORE INTO actors(user,host,harness,session) VALUES(?1,?2,?3,?4)",
        rusqlite::params![
            grantor.user,
            grantor.host,
            grantor.harness.as_str(),
            grantor.session
        ],
    )
    .unwrap();
    let grantor_id: i64 = conn
        .query_row(
            "SELECT id FROM actors WHERE user=?1 AND host=?2 AND harness=?3 AND session=?4",
            rusqlite::params![
                grantor.user,
                grantor.host,
                grantor.harness.as_str(),
                grantor.session
            ],
            |row| row.get(0),
        )
        .unwrap();
    conn.execute(
        "INSERT INTO agent_sessions(actor_id,first_seen,last_seen) VALUES(?1,?2,?2) ON CONFLICT(actor_id) DO NOTHING",
        rusqlite::params![grantor_id, now],
    )
    .unwrap();
    let seq: i64 = conn
        .query_row(
            "SELECT max(max_entry,max_event) FROM (SELECT COALESCE(MAX(seq),0) AS max_entry FROM entries), (SELECT COALESCE(MAX(seq),0) AS max_event FROM events)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let summary = format!("{role} fixture for {}", grantee.identity());
    let entry: i64 = conn
        .query_row(
            "INSERT INTO entries(plan_id,kind,body,to_whom,actor_id,seq,created_at,authority_basis) VALUES(1,'grant',?1,?2,?3,?4,?5,'owner') RETURNING id",
            rusqlite::params![summary, grantee.identity(), grantor_id, seq + 1, now],
            |row| row.get(0),
        )
        .unwrap();
    conn.execute(
        "INSERT INTO events(seq,plan_id,kind,subject,to_whom,actor_id,summary,created_at,authority_basis) VALUES(?1,1,'grant',?2,?3,?4,?5,?6,'owner')",
        rusqlite::params![seq + 1, format!("E{entry}"), grantee.identity(), grantor_id, summary, now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO authority_grants(entry_id,plan_id,owner_user,roles_json,grantee,granted_at,expires_at,revoked_by_entry,source) VALUES(?1,1,'josh',?2,?3,?4,NULL,NULL,'owner')",
        rusqlite::params![entry, format!("[\"{role}\"]"), grantee.identity(), now],
    )
    .unwrap();
    entry
}

fn assert_grant_role(conn: &Connection, entry: i64, role: &str) {
    let roles: String = conn
        .query_row(
            "SELECT roles_json FROM authority_grants WHERE entry_id=?1",
            [entry],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(roles, format!("[\"{role}\"]"));
}

fn assert_grant_authority(conn: &Connection, task: TaskId, entry: EntryId, grant: i64) {
    let entry_authority: (String, Option<i64>) = conn
        .query_row(
            "SELECT authority_basis,authority_entry FROM entries WHERE id=?1",
            [i64::try_from(entry.get()).unwrap()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let event_authority: (String, Option<i64>) = conn
        .query_row(
            "SELECT authority_basis,authority_entry FROM events WHERE subject=?1 ORDER BY seq DESC LIMIT 1",
            [task.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let expected = ("grant".to_owned(), Some(grant));
    assert_eq!(entry_authority, expected);
    assert_eq!(event_authority, expected);
}

fn assert_creator_authority(conn: &Connection, task: TaskId, entry: EntryId) {
    let entry_authority: (String, Option<i64>) = conn
        .query_row(
            "SELECT authority_basis,authority_entry FROM entries WHERE id=?1",
            [i64::try_from(entry.get()).unwrap()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let event_authority: (String, Option<i64>) = conn
        .query_row(
            "SELECT authority_basis,authority_entry FROM events WHERE subject=?1 ORDER BY seq DESC LIMIT 1",
            [task.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let expected = ("creator".to_owned(), None);
    assert_eq!(entry_authority, expected);
    assert_eq!(event_authority, expected);
}

fn create_unassigned_task(
    conn: &mut Connection,
    creator: &BoardActor,
    now: i64,
    title: &str,
) -> TaskId {
    let reply = write(conn, creator, now, |tx, ctx| {
        create_task(
            tx,
            ctx,
            PlanId::new(1).unwrap(),
            &PlanTitle::new(title).unwrap(),
            None,
            None,
        )
    })
    .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("expected task creation change")
    };
    change.task.unwrap()
}

fn create_task_for_actor(
    conn: &mut Connection,
    creator: &BoardActor,
    assignee: &BoardActor,
    now: i64,
    title: &str,
) -> TaskId {
    let recipient = BoardRecipient::for_actor(assignee);
    let reply = write(conn, creator, now, |tx, ctx| {
        create_task(
            tx,
            ctx,
            PlanId::new(1).unwrap(),
            &PlanTitle::new(title).unwrap(),
            Some(&recipient),
            None,
        )
    })
    .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("expected task creation change")
    };
    change.task.unwrap()
}
