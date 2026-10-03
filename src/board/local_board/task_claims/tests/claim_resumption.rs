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
        LocalBoard::open_path(&database.path, std::time::Duration::from_secs(120)).unwrap();
    // Keep the existing lease fresh when the backend uses the real clock.
    conn.execute(
        "UPDATE claims SET claimed_at=unixepoch(),last_active=unixepoch()",
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
    let history = read_claims(&conn, task.plan, i64::MAX, 120).unwrap();
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
        read_claims(&conn, task.plan, i64::MAX, 120).unwrap().len(),
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
        1000,
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
                    |tx, ctx| claim_task(tx, ctx, task, None, true),
                )
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap().unwrap();
    }
    let history = read_claims(&setup, task.plan, 1050, 120).unwrap();
    assert_eq!(history.len(), 3);
    assert_eq!(
        history
            .iter()
            .filter(|claim| claim.ended_at.is_none())
            .count(),
        1
    );
    assert!(
        history[..2]
            .iter()
            .all(|claim| claim.end_reason == Some(ClaimEndReason::Resumed))
    );
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
        1000,
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
                                false,
                            )
                        } else {
                            claim_task(tx, ctx, task, None, true)
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
    let history = read_claims(&setup, task.plan, 1121, 120).unwrap();
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
