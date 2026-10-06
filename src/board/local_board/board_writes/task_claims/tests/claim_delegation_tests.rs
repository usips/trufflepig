use super::*;

#[test]
fn delegated_claim_refreshes_on_holder_commits_not_delegator_commits() {
    let database = ClaimDatabase::new();
    let mut backend =
        LocalBoard::open_path(&database.path, std::time::Duration::from_secs(120)).unwrap();
    let orchestrator = actor("josh", "kimi", "orch");
    let coder = actor("josh", "codex", "c7");
    backend
        .handle(&BoardRequest::new(
            coder.clone(),
            BoardOp::Hello {
                model: "Codex".into(),
                effort: Some("xhigh".into()),
            },
        ))
        .unwrap();
    let task = TaskId::new(PlanId::new(1).unwrap(), 1).unwrap();
    backend
        .handle(&BoardRequest::new(
            orchestrator.clone(),
            BoardOp::TaskCreate {
                plan: task.plan,
                title: PlanTitle::new("Delegated lane").unwrap(),
                to: None,
                section: None,
            },
        ))
        .unwrap();
    backend
        .handle(&BoardRequest::new(
            orchestrator.clone(),
            BoardOp::ClaimTask {
                task,
                scope: Some(EntryText::new("coder lane").unwrap()),
                resume: ClaimResume::No,
                delegate: Some(ClaimDelegate {
                    harness: HarnessLabel::parse("codex").unwrap(),
                    session: "c7".into(),
                }),
            },
        ))
        .unwrap();
    let mut conn = database.connect();
    let history = read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, i64::MAX, 120).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].actor, coder);
    assert_eq!(history[0].delegated_by, Some(orchestrator.clone()));
    assert_eq!(history[0].model.as_deref(), Some("Codex"));
    let entry = crate::board::local_board::read_entry(&conn, history[0].entry).unwrap();
    assert_eq!(entry.actor, coder);
    let (event_actor, to_whom, summary): (String, Option<String>, String) = conn
        .query_row(
            "SELECT a.user||'@'||a.host||'/'||a.harness||'/'||a.session,e.to_whom,e.summary FROM events e JOIN actors a ON a.id=e.actor_id WHERE e.subject=?1",
            [history[0].entry.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(event_actor, orchestrator.identity());
    assert_eq!(to_whom.as_deref(), Some(coder.identity()).as_deref());
    assert!(
        summary.contains(&format!("(via {})", orchestrator.identity())),
        "{summary}"
    );
    let claimed_at = history[0].claimed_at;
    let coauthor = |harness: &str| CommitCoauthor {
        harness: HarnessLabel::parse(harness).unwrap(),
        model: "trailer model".into(),
        email: "agent@example.invalid".into(),
    };
    write(&mut conn, &coder, claimed_at + 100, |tx, ctx| {
        refresh_commit_claims(
            tx,
            task.plan,
            task.ordinal,
            &[coauthor("codex")],
            claimed_at + 50,
            ctx.now,
        )?;
        Ok(ctx.change_reply(EntryId::new(1).unwrap(), Some(task.plan), None, Some(task)))
    })
    .unwrap();
    write(&mut conn, &orchestrator, claimed_at + 100, |tx, ctx| {
        refresh_commit_claims(
            tx,
            task.plan,
            task.ordinal,
            &[coauthor("kimi")],
            claimed_at + 80,
            ctx.now,
        )?;
        Ok(ctx.change_reply(EntryId::new(1).unwrap(), Some(task.plan), None, Some(task)))
    })
    .unwrap();
    let history =
        read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, claimed_at + 100, 120).unwrap();
    assert_eq!(history[0].last_active, claimed_at + 50);
    let reply = backend
        .handle(&BoardRequest::new(
            orchestrator,
            BoardOp::Claims {
                plan: Some(task.plan),
                own_stale: false,
                repo_key: None,
                all: true,
                after: None,
                through: None,
                limit: 200,
            },
        ))
        .unwrap();
    let BoardResult::Claims(page) = reply.result else {
        panic!("expected claims");
    };
    assert_eq!(page.claims.len(), 1);
    assert_eq!(page.claims[0].claim.actor, coder);
    assert_eq!(page.claims[0].claim.delegated_by, Some(coder_delegator()));
}

fn coder_delegator() -> BoardActor {
    actor("josh", "kimi", "orch")
}

#[test]
fn delegated_claim_event_reaches_delegate_inbox_with_via_summary() {
    let database = ClaimDatabase::new();
    let mut backend =
        LocalBoard::open_path(&database.path, std::time::Duration::from_secs(120)).unwrap();
    let orchestrator = actor("josh", "kimi", "orch");
    let coder = actor("josh", "codex", "c7");
    backend
        .handle(&BoardRequest::new(
            coder.clone(),
            BoardOp::Hello {
                model: "Codex".into(),
                effort: Some("xhigh".into()),
            },
        ))
        .unwrap();
    // Backdate the delegate's activity so the no-bump check is exact.
    database
        .connect()
        .execute("UPDATE agent_sessions SET last_seen=1000", [])
        .unwrap();
    let task = TaskId::new(PlanId::new(1).unwrap(), 1).unwrap();
    backend
        .handle(&BoardRequest::new(
            orchestrator.clone(),
            BoardOp::TaskCreate {
                plan: task.plan,
                title: PlanTitle::new("Delegated lane").unwrap(),
                to: None,
                section: None,
            },
        ))
        .unwrap();
    let reply = backend
        .handle(&BoardRequest::new(
            orchestrator.clone(),
            BoardOp::ClaimTask {
                task,
                scope: Some(EntryText::new("coder lane").unwrap()),
                resume: ClaimResume::No,
                delegate: Some(ClaimDelegate {
                    harness: HarnessLabel::parse("codex").unwrap(),
                    session: "c7".into(),
                }),
            },
        ))
        .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("expected change");
    };
    let conn = database.connect();
    let (event_actor, to_whom, summary): (String, Option<String>, String) = conn
        .query_row(
            "SELECT a.user||'@'||a.host||'/'||a.harness||'/'||a.session,e.to_whom,e.summary FROM events e JOIN actors a ON a.id=e.actor_id WHERE e.subject=?1",
            [change.entry.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(event_actor, orchestrator.identity());
    assert_eq!(to_whom.as_deref(), Some(coder.identity()).as_deref());
    assert!(
        summary.contains(&format!("(via {})", orchestrator.identity())),
        "{summary}"
    );
    let entry = crate::board::local_board::read_entry(&conn, change.entry).unwrap();
    assert_eq!(entry.actor, coder);
    let last_seen: i64 = conn
        .query_row(
            "SELECT last_seen FROM agent_sessions WHERE actor_id=(SELECT id FROM actors WHERE user=?1 AND host=?2 AND harness=?3 AND session=?4)",
            rusqlite::params![coder.user, coder.host, coder.harness.as_str(), coder.session],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(last_seen, 1000);
    let reply = backend
        .handle(&BoardRequest::new(
            coder.clone(),
            BoardOp::Inbox {
                after: None,
                limit: 50,
                repo_key: None,
                all: false,
            },
        ))
        .unwrap();
    let BoardResult::Inbox(inbox) = reply.result else {
        panic!("expected inbox");
    };
    let event = inbox
        .events
        .iter()
        .find(|event| event.subject.to_string() == change.entry.to_string())
        .unwrap_or_else(|| panic!("delegate inbox lacks the claim: {:?}", inbox.events));
    assert_eq!(event.actor, orchestrator);
    assert_eq!(event.to, Some(BoardRecipient::for_actor(&coder)));
}

#[test]
fn delegation_takeover_notifies_the_stale_holder() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let old = actor("josh", "muse", "old");
    let task = carved_task(&mut conn, &old, 1000, "stale scope");
    let orchestrator = actor("josh", "kimi", "orch");
    write(&mut conn, &orchestrator, 1121, |tx, ctx| {
        claim_task(
            tx,
            ctx,
            task,
            Some(&EntryText::new("delegated lane").unwrap()),
            ClaimResume::No,
            Some(&ClaimDelegate {
                harness: HarnessLabel::parse("codex").unwrap(),
                session: "c7".into(),
            }),
        )
    })
    .unwrap();
    let (recipient, summary): (String, String) = conn
        .query_row(
            "SELECT to_whom,summary FROM events ORDER BY seq DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(recipient, old.identity());
    assert!(summary.contains("took over stale claim from"), "{summary}");
}

#[test]
fn unseen_delegate_gets_no_last_seen() {
    let database = ClaimDatabase::new();
    let mut conn = database.connect();
    let orchestrator = actor("josh", "kimi", "orch");
    let coder = actor("josh", "codex", "c7");
    let reply = write(&mut conn, &orchestrator, 1000, |tx, ctx| {
        create_task(
            tx,
            ctx,
            PlanId::new(1).unwrap(),
            &PlanTitle::new("Delegated lane").unwrap(),
            None,
            None,
        )
    })
    .unwrap();
    let BoardResult::Change(created) = reply.result else {
        panic!("expected change")
    };
    let task = created.task.unwrap();
    write(&mut conn, &orchestrator, 1001, |tx, ctx| {
        claim_task(
            tx,
            ctx,
            task,
            Some(&EntryText::new("coder lane").unwrap()),
            ClaimResume::No,
            Some(&ClaimDelegate {
                harness: HarnessLabel::parse("codex").unwrap(),
                session: "c7".into(),
            }),
        )
    })
    .unwrap();
    let last_seen: i64 = conn
        .query_row(
            "SELECT last_seen FROM agent_sessions WHERE actor_id=(SELECT id FROM actors WHERE user=?1 AND host=?2 AND harness=?3 AND session=?4)",
            rusqlite::params![coder.user, coder.host, coder.harness.as_str(), coder.session],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(last_seen, 0, "a delegate that never acted was never seen");
}
