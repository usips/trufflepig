use super::*;

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
        Ok(ctx.change_reply(EntryId::new(1).unwrap(), Some(own.plan), None, Some(own)))
    })
    .unwrap();
    let claims = read_claims_window(&conn, own.plan, i64::MIN, i64::MAX, 1050, 120).unwrap();
    assert_eq!(claims[0].last_active, 1050);
    assert_eq!(claims[1].last_active, 1000);
    write(&mut conn, &holder, 1080, |tx, ctx| {
        refresh_inbox_claims(tx, ctx.actor_id, ctx.now)?;
        Ok(ctx.change_reply(EntryId::new(1).unwrap(), Some(own.plan), None, Some(own)))
    })
    .unwrap();
    assert_eq!(
        read_claims_window(&conn, other.plan, i64::MIN, i64::MAX, 1080, 120).unwrap()[0]
            .last_active,
        1080
    );
    write(&mut conn, &holder, 1090, |tx, ctx| {
        move_task(tx, ctx, own, TaskColumn::Review, None)
    })
    .unwrap();
    write(&mut conn, &holder, 1200, |tx, ctx| {
        refresh_inbox_claims(tx, ctx.actor_id, ctx.now)?;
        Ok(ctx.change_reply(EntryId::new(1).unwrap(), Some(own.plan), None, Some(own)))
    })
    .unwrap();
    assert_eq!(
        read_claims_window(&conn, own.plan, i64::MIN, i64::MAX, 1200, 120).unwrap()[0].last_active,
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
            Ok(ctx.change_reply(EntryId::new(1).unwrap(), Some(task.plan), None, Some(task)))
        })
        .unwrap();
    }
    assert_eq!(
        read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, 1100, 120).unwrap()[0].last_active,
        1000
    );
    write(&mut conn, &holder, 1100, |tx, ctx| {
        refresh_commit_claims(tx, task.plan, task.ordinal, &[coauthor.clone()], 1050, 1100)?;
        Ok(ctx.change_reply(EntryId::new(1).unwrap(), Some(task.plan), None, Some(task)))
    })
    .unwrap();
    assert_eq!(
        read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, 1100, 120).unwrap()[0].last_active,
        1050
    );
    write(&mut conn, &holder, 1110, |tx, ctx| {
        move_task(tx, ctx, task, TaskColumn::Done, None)
    })
    .unwrap();
    write(&mut conn, &holder, 1150, |tx, ctx| {
        refresh_commit_claims(tx, task.plan, task.ordinal, &[coauthor], 1120, 1150)?;
        Ok(ctx.change_reply(EntryId::new(1).unwrap(), Some(task.plan), None, Some(task)))
    })
    .unwrap();
    assert_eq!(
        read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, 1150, 120).unwrap()[0].last_active,
        1050
    );
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
            Ok(ctx.change_reply(EntryId::new(1).unwrap(), Some(task.plan), None, Some(task)))
        })
        .unwrap();
    }
    let claim = &read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, 2000, 120).unwrap()[0];
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
        conn.execute(
            "UPDATE entries SET model=?1 WHERE id=(SELECT entry_id FROM claims WHERE plan_id=?2 AND task_ordinal=?3)",
            params![model, sql_number(task.plan.get()), sql_number(task.ordinal)],
        )
        .unwrap();
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
            Ok(ctx.change_reply(EntryId::new(1).unwrap(), Some(task.plan), None, Some(task)))
        })
        .unwrap();
        let history = read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, 1100, 120).unwrap();
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
fn muse_claim_refreshes_from_meta_com_coauthor() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let holder = actor("josh", "muse", "one");
    let task = carved_task(&mut conn, &holder, 1000, "current");
    // The hello model snapshot claims Muse; no vendor prefix matches it, so
    // the claim vendor falls back to the muse harness.
    conn.execute(
        "UPDATE entries SET model='Muse Spark' WHERE id=(SELECT entry_id FROM claims WHERE plan_id=?1 AND task_ordinal=?2)",
        params![sql_number(task.plan.get()), sql_number(task.ordinal)],
    )
    .unwrap();
    let coauthor =
        crate::board::commit_trailers::parse_coauthor("Muse Spark <noreply@meta.com>").unwrap();
    write(&mut conn, &holder, 1100, |tx, ctx| {
        refresh_commit_claims(tx, task.plan, task.ordinal, &[coauthor], 1050, ctx.now)?;
        Ok(ctx.change_reply(EntryId::new(1).unwrap(), Some(task.plan), None, Some(task)))
    })
    .unwrap();
    assert_eq!(
        read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, 1100, 120).unwrap()[0].last_active,
        1050
    );
}
