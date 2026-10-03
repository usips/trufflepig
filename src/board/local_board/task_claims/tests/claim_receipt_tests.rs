use super::*;

#[test]
fn cached_claim_or_move_cannot_bypass_a_new_holder() {
    let database = ClaimDatabase::new();
    let mut backend =
        LocalBoard::open_path(&database.path, std::time::Duration::from_secs(120)).unwrap();
    let original = actor("josh", "codex", "one");
    let next = actor("josh", "muse", "two");
    let task = TaskId::new(PlanId::new(1).unwrap(), 1).unwrap();
    backend
        .handle(&BoardRequest::new(
            original.clone(),
            BoardOp::TaskCreate {
                plan: task.plan,
                title: PlanTitle::new("Retry safety").unwrap(),
                to: None,
                section: None,
            },
        ))
        .unwrap();
    let claim_request = BoardRequest::new(
        original.clone(),
        BoardOp::ClaimTask {
            task,
            scope: Some(EntryText::new("my lane").unwrap()),
            resume: false,
        },
    );
    backend.handle(&claim_request).unwrap();
    let release_request = BoardRequest::new(
        original,
        BoardOp::TaskMove {
            task,
            column: TaskColumn::Review,
            to: None,
        },
    );
    backend.handle(&release_request).unwrap();
    backend
        .handle(&BoardRequest::new(
            next.clone(),
            BoardOp::ClaimTask {
                task,
                scope: Some(EntryText::new("new holder").unwrap()),
                resume: false,
            },
        ))
        .unwrap();
    let claim_error = backend.handle(&claim_request).unwrap_err();
    assert!(claim_error.to_string().starts_with("claim_conflict:"));
    let move_error = backend.handle(&release_request).unwrap_err();
    assert!(move_error.to_string().starts_with("invalid_actor:"));
    let conn = database.connect();
    let history = read_claims_window(&conn, task.plan, i64::MIN, i64::MAX, i64::MAX, 120).unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|claim| claim.ended_at.is_none())
            .count(),
        1
    );
    assert_eq!(history.last().unwrap().actor, next);
}

#[test]
fn retried_carve_creates_fresh_task_after_handoff() {
    let database = ClaimDatabase::new();
    let mut backend =
        LocalBoard::open_path(&database.path, std::time::Duration::from_secs(120)).unwrap();
    let original = actor("josh", "codex", "one");
    let task = TaskId::new(PlanId::new(1).unwrap(), 1).unwrap();
    let request = BoardRequest::new(
        original.clone(),
        BoardOp::CarveClaim {
            plan: task.plan,
            title: PlanTitle::new("Atomic carve").unwrap(),
            scope: EntryText::new("my scope").unwrap(),
            section: Some("Claims".into()),
        },
    );
    backend.handle(&request).unwrap();
    backend
        .handle(&BoardRequest::new(
            original,
            BoardOp::TaskMove {
                task,
                column: TaskColumn::Todo,
                to: None,
            },
        ))
        .unwrap();
    backend
        .handle(&BoardRequest::new(
            actor("josh", "muse", "two"),
            BoardOp::ClaimTask {
                task,
                scope: Some(EntryText::new("next scope").unwrap()),
                resume: false,
            },
        ))
        .unwrap();
    let reply = backend.handle(&request).unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("expected change")
    };
    assert_eq!(change.task.unwrap().ordinal, 2);
    assert!(!change.deduplicated);
    assert_eq!(read_tasks(&database.connect(), task.plan).unwrap().len(), 2);
    let claims = read_claims_window(
        &database.connect(),
        task.plan,
        i64::MIN,
        i64::MAX,
        i64::MAX,
        120,
    )
    .unwrap();
    assert_eq!(
        claims
            .iter()
            .filter(|claim| claim.ended_at.is_none())
            .count(),
        2
    );
    assert_eq!(
        claims
            .iter()
            .find(|claim| claim.task == task && claim.ended_at.is_none())
            .unwrap()
            .actor
            .harness
            .as_str(),
        "muse"
    );
}

#[test]
fn carve_retry_deduplicates_current_lease_but_creates_fresh_after_done() {
    let database = ClaimDatabase::new();
    let mut backend =
        LocalBoard::open_path(&database.path, std::time::Duration::from_secs(120)).unwrap();
    let holder = actor("josh", "codex", "one");
    let request = BoardRequest::new(
        holder.clone(),
        BoardOp::CarveClaim {
            plan: PlanId::new(1).unwrap(),
            title: PlanTitle::new("Carve retry").unwrap(),
            scope: EntryText::new("scope").unwrap(),
            section: None,
        },
    );
    let first = backend.handle(&request).unwrap();
    let BoardResult::Change(first) = first.result else {
        panic!("expected change")
    };
    let replay = backend.handle(&request).unwrap();
    let BoardResult::Change(replay) = replay.result else {
        panic!("expected change")
    };
    assert!(replay.deduplicated);
    assert_eq!(first.task, replay.task);
    backend
        .handle(&BoardRequest::new(
            holder,
            BoardOp::TaskMove {
                task: first.task.unwrap(),
                column: TaskColumn::Done,
                to: None,
            },
        ))
        .unwrap();
    let retried = backend.handle(&request).unwrap();
    let BoardResult::Change(retried) = retried.result else {
        panic!("expected change")
    };
    assert_ne!(first.task, retried.task);
    assert!(!retried.deduplicated);
    let tasks = read_tasks(&database.connect(), first.plan.unwrap()).unwrap();
    assert_eq!(tasks[0].column, TaskColumn::Done);
    assert_eq!(tasks[1].column, TaskColumn::Doing);
}
