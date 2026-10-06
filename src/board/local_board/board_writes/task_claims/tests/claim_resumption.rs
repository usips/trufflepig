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
fn own_resume_returns_the_claim_entry_seq() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "muse", "worktree-one");
    let task = carved_task(&mut conn, &holder, 1000, "live lane");
    let before = active_claim(&conn, task, 1000, 120)
        .unwrap()
        .unwrap()
        .record;
    let entry_seq = conn
        .query_row(
            "SELECT seq FROM entries WHERE id=?1",
            [sql_number(before.entry.get())],
            |row| row_number(row, 0),
        )
        .unwrap();
    let reply = write(&mut conn, &holder, 1050, |tx, ctx| {
        claim_task(tx, ctx, task, None, ClaimResume::Idle, None)
    })
    .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("expected change")
    };
    assert_eq!(change.entry, before.entry);
    assert_eq!(change.seq.get(), entry_seq);
}
