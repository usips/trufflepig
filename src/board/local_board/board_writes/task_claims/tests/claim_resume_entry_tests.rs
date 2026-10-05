use super::*;

#[test]
fn explicit_resume_entry_takes_over_a_live_claim_immediately() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "muse", "worktree-one");
    let task = carved_task(&mut conn, &holder, 1000, "live lane");
    let current = active_claim(&conn, task, 1050, 120)
        .unwrap()
        .unwrap()
        .record;
    assert!(!current.stale);
    let other = actor("josh", "muse", "worktree-two");
    for wrong in [
        EntryId::new(999).unwrap(),
        EntryId::new(current.entry.get() + 1).unwrap(),
    ] {
        let error = write(&mut conn, &other, 1050, |tx, ctx| {
            claim_task(tx, ctx, task, None, ClaimResume::Entry(wrong), None)
        })
        .unwrap_err()
        .to_string();
        assert!(error.starts_with("invalid_reference:"), "{error}");
    }
    // Naming the exact claim entry asserts deliberate intent: no idle wait.
    write(&mut conn, &other, 1050, |tx, ctx| {
        claim_task(tx, ctx, task, None, ClaimResume::Entry(current.entry), None)
    })
    .unwrap();
    let history = read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, 1050, 120).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].end_reason, Some(ClaimEndReason::Resumed));
    assert_eq!(history[1].actor, other);
    assert_eq!(history[1].scope.as_str(), "live lane");
    assert!(history[1].ended_at.is_none());
    // An ended claim's entry no longer names the current claim.
    let ended = history[0].entry;
    let error = write(&mut conn, &holder, 1060, |tx, ctx| {
        claim_task(tx, ctx, task, None, ClaimResume::Entry(ended), None)
    })
    .unwrap_err()
    .to_string();
    assert!(error.starts_with("invalid_reference:"), "{error}");
}

#[test]
fn explicit_resume_entry_still_requires_same_user_host_and_harness() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "muse", "worktree-one");
    let task = carved_task(&mut conn, &holder, 1000, "exclusive");
    let current = active_claim(&conn, task, 1050, 120)
        .unwrap()
        .unwrap()
        .record;
    for replacement in [
        actor("other", "muse", "worktree-two"),
        actor("josh", "codex", "worktree-two"),
        BoardActor::new(
            "josh",
            "other-host",
            HarnessLabel::parse("muse").unwrap(),
            "worktree-two",
        )
        .unwrap(),
    ] {
        let error = write(&mut conn, &replacement, 1050, |tx, ctx| {
            claim_task(tx, ctx, task, None, ClaimResume::Entry(current.entry), None)
        })
        .unwrap_err()
        .to_string();
        assert!(error.starts_with("invalid_actor:"), "{error}");
    }
    assert_eq!(
        read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, 1050, 120)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn show_lists_the_claim_entry_for_resume_targeting() {
    let directory = crate::board::board_test_support::scratch("board-test-");
    let mut backend = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        std::time::Duration::from_secs(7200),
    )
    .unwrap();
    let holder = actor("josh", "muse", "worktree-one");
    let BoardResult::Change(plan) = backend
        .handle(&BoardRequest::new(
            holder.clone(),
            BoardOp::New {
                title: PlanTitle::new("Claims").unwrap(),
                body: crate::board::board_vocabulary::PlanText::new("# Scope").unwrap(),
                steward: None,
                repo_key: None,
            },
        ))
        .unwrap()
        .result
    else {
        panic!("expected plan")
    };
    let BoardResult::Change(carve) = backend
        .handle(&BoardRequest::new(
            holder.clone(),
            BoardOp::CarveClaim {
                plan: plan.plan.unwrap(),
                title: PlanTitle::new("discoverable entry").unwrap(),
                scope: EntryText::new("live lane").unwrap(),
                section: None,
            },
        ))
        .unwrap()
        .result
    else {
        panic!("expected carve")
    };
    let reply = backend
        .handle(&BoardRequest::new(
            holder,
            BoardOp::Show {
                target: crate::board::board_ids::BoardRef::Task(carve.task.unwrap()),
            },
        ))
        .unwrap();
    let json = serde_json::to_value(&reply.result).unwrap();
    assert!(
        json["data"]["claims"]
            .as_array()
            .unwrap()
            .iter()
            .any(|claim| claim["entry"] == carve.entry.to_string()),
        "show exposes the claim entry id: {json}"
    );
}
