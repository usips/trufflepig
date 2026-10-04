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
    let event_actor: String = conn
        .query_row(
            "SELECT a.user||'@'||a.host||'/'||a.harness||'/'||a.session FROM events e JOIN actors a ON a.id=e.actor_id WHERE e.subject=?1",
            [history[0].entry.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(event_actor, coder.identity());
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
fn foreign_user_delegation_is_rejected() {
    let database = ClaimDatabase::new();
    let mut backend =
        LocalBoard::open_path(&database.path, std::time::Duration::from_secs(120)).unwrap();
    let outsider = actor("mallory", "codex", "s1");
    let task = TaskId::new(PlanId::new(1).unwrap(), 1).unwrap();
    backend
        .handle(&BoardRequest::new(
            actor("josh", "codex", "owner"),
            BoardOp::TaskCreate {
                plan: task.plan,
                title: PlanTitle::new("Owned lane").unwrap(),
                to: None,
                section: None,
            },
        ))
        .unwrap();
    let error = backend
        .handle(&BoardRequest::new(
            outsider,
            BoardOp::ClaimTask {
                task,
                scope: Some(EntryText::new("intruding lane").unwrap()),
                resume: ClaimResume::No,
                delegate: Some(ClaimDelegate {
                    harness: HarnessLabel::parse("codex").unwrap(),
                    session: "c7".into(),
                }),
            },
        ))
        .unwrap_err()
        .to_string();
    assert!(error.starts_with("invalid_actor:"), "{error}");
    let conn = database.connect();
    assert!(
        read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, i64::MAX, 120)
            .unwrap()
            .is_empty()
    );
}
