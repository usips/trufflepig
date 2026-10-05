use super::*;

#[test]
fn resumed_session_preserves_scope_and_records_previous_session() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let previous = actor("josh", "muse", "interrupted");
    let task = carved_task(&mut conn, &previous, 1000, "parser and tests");
    let replacement = actor("josh", "muse", "resumed");
    assert!(claim(&mut conn, &replacement, 1050, task, "scope").is_err());
    let mut backend =
        LocalBoard::open_path(&database.path, std::time::Duration::from_secs(7200)).unwrap();
    // Idle the lease past the resume grace window on the backend's real clock.
    conn.execute(
        "UPDATE claims SET claimed_at=unixepoch()-700,last_active=unixepoch()-700",
        [],
    )
    .unwrap();
    let op: BoardOp = serde_json::from_value(serde_json::json!({
        "op": "claim_task", "task": task.to_string(), "scope": null, "resume": true
    }))
    .unwrap();
    let request = BoardRequest::new(replacement.clone(), op);
    let first = backend.handle(&request).unwrap();
    let replay = backend.handle(&request).unwrap();
    let BoardResult::Change(first) = first.result else {
        panic!("expected change")
    };
    let BoardResult::Change(replay) = replay.result else {
        panic!("expected change")
    };
    assert!(replay.deduplicated);
    assert_eq!(first.entry, replay.entry);
    let history = read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, i64::MAX, 120).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].actor, previous);
    assert!(history[0].ended_at.is_some());
    assert_eq!(
        serde_json::to_value(history[0].end_reason).unwrap(),
        "resumed"
    );
    assert_eq!(history[1].actor, replacement);
    assert_eq!(history[1].scope.as_str(), "parser and tests");
    assert!(history[1].ended_at.is_none());
    let summary: String = conn
        .query_row(
            "SELECT summary FROM events ORDER BY seq DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(summary.contains("resumed"));
    assert!(summary.contains(&previous.identity()));
}

#[test]
fn resume_requires_same_user_host_and_harness() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let previous = actor("josh", "muse", "interrupted");
    let task = carved_task(&mut conn, &previous, 1000, "exclusive");
    let mut backend =
        LocalBoard::open_path(&database.path, std::time::Duration::from_secs(120)).unwrap();
    for replacement in [
        actor("other", "muse", "new"),
        actor("josh", "codex", "new"),
        BoardActor::new(
            "josh",
            "other-host",
            HarnessLabel::parse("muse").unwrap(),
            "new",
        )
        .unwrap(),
    ] {
        let op: BoardOp = serde_json::from_value(serde_json::json!({
            "op": "claim_task", "task": task.to_string(), "scope": null, "resume": true
        }))
        .unwrap();
        let error = backend
            .handle(&BoardRequest::new(replacement, op))
            .unwrap_err();
        assert!(error.to_string().starts_with("invalid_actor:"), "{error}");
    }
    assert_eq!(
        read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, i64::MAX, 120)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn simultaneous_resumes_serialize_history_and_leave_one_current_session() {
    let database = ClaimDatabase::new();
    let mut setup = database.connect();
    let task = carved_task(
        &mut setup,
        &actor("josh", "muse", "interrupted"),
        400,
        "resume scope",
    );
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = [database.connect(), database.connect()]
        .into_iter()
        .enumerate()
        .map(|(index, mut conn)| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                write(
                    &mut conn,
                    &actor("josh", "muse", &format!("resumed-{index}")),
                    1050,
                    |tx, ctx| claim_task(tx, ctx, task, None, ClaimResume::Idle, None),
                )
            })
        })
        .collect();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let loser = results.iter().find_map(|result| result.as_ref().err());
    assert!(
        loser
            .map(|error| error.to_string())
            .is_some_and(|error| error.starts_with("claim_conflict:")),
        "the live resumed lease refuses the racing bare resume: {loser:?}"
    );
    let history = read_claims_window(&setup, task.plan, i64::MIN, i64::MAX, 1050, 120).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(
        history
            .iter()
            .filter(|claim| claim.ended_at.is_none())
            .count(),
        1
    );
    assert_eq!(history[0].end_reason, Some(ClaimEndReason::Resumed));
    assert!(
        history
            .iter()
            .all(|claim| claim.scope.as_str() == "resume scope")
    );
}

#[test]
fn resume_racing_stale_takeover_cannot_bypass_new_holders_identity() {
    let database = ClaimDatabase::new();
    let mut setup = database.connect();
    let task = carved_task(
        &mut setup,
        &actor("josh", "muse", "interrupted"),
        400,
        "old scope",
    );
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = [database.connect(), database.connect()]
        .into_iter()
        .enumerate()
        .map(|(index, mut conn)| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let harness = if index == 0 { "codex" } else { "muse" };
                barrier.wait();
                write(
                    &mut conn,
                    &actor("josh", harness, "replacement"),
                    1121,
                    |tx, ctx| {
                        if index == 0 {
                            claim_task(
                                tx,
                                ctx,
                                task,
                                Some(&EntryText::new("takeover scope").unwrap()),
                                ClaimResume::No,
                                None,
                            )
                        } else {
                            claim_task(tx, ctx, task, None, ClaimResume::Idle, None)
                        }
                    },
                )
            })
        })
        .collect();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let history = read_claims_window(&setup, task.plan, i64::MIN, i64::MAX, 1121, 120).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(
        history
            .iter()
            .filter(|claim| claim.ended_at.is_none())
            .count(),
        1
    );
    assert_eq!(history[1].claimed_at, 1121);
}

#[test]
fn bare_resume_requires_idle_grace_before_replacing_a_live_claim() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "muse", "worktree-one");
    let task = carved_task(&mut conn, &holder, 1000, "live lane");
    let other = actor("josh", "muse", "worktree-two");
    let refused = write(&mut conn, &other, 1050, |tx, ctx| {
        claim_task(tx, ctx, task, None, ClaimResume::Idle, None)
    })
    .unwrap_err()
    .to_string();
    assert!(refused.starts_with("claim_conflict:"), "{refused}");
    assert!(refused.contains(&holder.identity()), "{refused}");
    assert!(refused.contains("test-model/xhigh"), "{refused}");
    assert!(refused.contains("active 50s ago"), "{refused}");
    assert_eq!(
        read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, 1050, 120)
            .unwrap()
            .len(),
        1
    );
    // Idling the lease past the grace window lets the same resume through.
    conn.execute("UPDATE claims SET last_active=440", [])
        .unwrap();
    write(&mut conn, &other, 1050, |tx, ctx| {
        claim_task(tx, ctx, task, None, ClaimResume::Idle, None)
    })
    .unwrap();
    let history = read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, 1050, 120).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].end_reason, Some(ClaimEndReason::Resumed));
    assert_eq!(history[1].actor, other);
    assert!(history[1].ended_at.is_none());
}

#[test]
fn bare_resume_of_own_live_claim_refreshes_without_conflict() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "muse", "worktree-one");
    let task = carved_task(&mut conn, &holder, 1000, "live lane");
    let before = active_claim(&conn, task, 1000, 120)
        .unwrap()
        .unwrap()
        .record;
    let entries_before: i64 = conn
        .query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))
        .unwrap();
    let events_before: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();
    let reply = write(&mut conn, &holder, 1050, |tx, ctx| {
        claim_task(tx, ctx, task, None, ClaimResume::Idle, None)
    })
    .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("expected change")
    };
    assert_eq!(change.entry, before.entry);
    let history = read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, 1050, 120).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].entry, before.entry);
    assert_eq!(history[0].actor, holder);
    assert_eq!(history[0].scope.as_str(), "live lane");
    assert_eq!(history[0].claimed_at, 1000);
    assert_eq!(history[0].last_active, 1050);
    assert!(history[0].ended_at.is_none());
    assert!(history[0].end_reason.is_none());
    let entries_after: i64 = conn
        .query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))
        .unwrap();
    let events_after: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(entries_after, entries_before);
    assert_eq!(events_after, events_before);
}

#[test]
fn pre_resume_commit_refreshes_after_own_holder_resume() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "codex", "one");
    let task = carved_task(&mut conn, &holder, 1000, "current");
    let before = active_claim(&conn, task, 1000, 120)
        .unwrap()
        .unwrap()
        .record;
    // An own-holder resume with a scope update keeps the original claim row.
    let reply = write(&mut conn, &holder, 1050, |tx, ctx| {
        claim_task(
            tx,
            ctx,
            task,
            Some(&EntryText::new("resumed lane").unwrap()),
            ClaimResume::Idle,
            None,
        )
    })
    .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("expected change")
    };
    assert_eq!(change.entry, before.entry);
    let history = read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, 1050, 120).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].claimed_at, 1000);
    assert_eq!(history[0].scope.as_str(), "resumed lane");
    // Idle below the commit time so the commit gate is observable, then
    // ingest a commit predating the resume: claimed_at never moved, so the
    // commit still counts as lease activity.
    conn.execute("UPDATE claims SET last_active=1010", [])
        .unwrap();
    let coauthor = CommitCoauthor {
        harness: HarnessLabel::parse("codex").unwrap(),
        model: "Codex".into(),
        email: "agent@openai.com".into(),
    };
    write(&mut conn, &holder, 1100, |tx, ctx| {
        refresh_commit_claims(tx, task.plan, task.ordinal, &[coauthor], 1020, ctx.now)?;
        Ok(ctx.change_reply(EntryId::new(1).unwrap(), Some(task.plan), None, Some(task)))
    })
    .unwrap();
    let history = read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, 1100, 120).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].last_active, 1020);
}

#[test]
fn own_holder_resume_returns_existing_entry_through_dispatch() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "muse", "worktree-one");
    let task = carved_task(&mut conn, &holder, 1000, "live lane");
    let before = active_claim(&conn, task, 1000, 120)
        .unwrap()
        .unwrap()
        .record;
    let events_before: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();
    let mut backend =
        LocalBoard::open_path(&database.path, std::time::Duration::from_secs(7200)).unwrap();
    let op: BoardOp = serde_json::from_value(serde_json::json!({
        "op": "claim_task", "task": task.to_string(), "scope": null, "resume": true
    }))
    .unwrap();
    let reply = backend
        .handle(&BoardRequest::new(holder.clone(), op))
        .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("expected change")
    };
    assert_eq!(change.entry, before.entry);
    assert_eq!(change.task, Some(task));
    let history = read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, i64::MAX, 120).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].claimed_at, 1000);
    assert!(history[0].last_active > 1000);
    let events_after: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(events_after, events_before);
}

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
