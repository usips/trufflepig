use super::*;

#[test]
fn delegator_releases_delegated_claim_by_moving_task() {
    let database = ClaimDatabase::new();
    let mut backend =
        LocalBoard::open_path(&database.path, std::time::Duration::from_secs(120)).unwrap();
    let orchestrator = actor("josh", "kimi", "orch");
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
    let error = backend
        .handle(&BoardRequest::new(
            actor("josh", "muse", "two"),
            BoardOp::TaskMove {
                task,
                column: TaskColumn::Review,
                to: None,
            },
        ))
        .unwrap_err()
        .to_string();
    assert!(error.starts_with("invalid_actor:"), "{error}");
    backend
        .handle(&BoardRequest::new(
            orchestrator,
            BoardOp::TaskMove {
                task,
                column: TaskColumn::Todo,
                to: None,
            },
        ))
        .unwrap();
    let conn = database.connect();
    let claims = read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, i64::MAX, 120).unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].end_reason, Some(ClaimEndReason::Released));
    assert!(claims[0].ended_at.is_some());
    assert_eq!(
        read_tasks(&conn, task.plan).unwrap()[0].column,
        TaskColumn::Todo
    );
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
