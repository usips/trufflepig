use super::*;

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
