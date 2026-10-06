use super::*;

#[test]
fn manual_commit_link_records_manual_source_and_names_the_linker() {
    let directory = crate::board::board_test_support::scratch("board-fixture-");
    let mut board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    let (owner, task) = manual_link_plan(&mut board);
    let reply = board
        .handle(&BoardRequest::new(
            owner.clone(),
            BoardOp::LinkCommit {
                oid: crate::identity::GitOid::parse(&"b".repeat(40)).unwrap(),
                task,
                resolution: Some(Box::new(manual_link_resolution())),
            },
        ))
        .unwrap();
    let BoardResult::Change(change) = &reply.result else {
        panic!("change result: {reply:?}")
    };
    assert!(!change.deduplicated);
    assert_eq!(change.task, Some(task));
    assert_eq!(change.plan, Some(task.plan));
    assert_eq!(
        manual_link_count(
            &board,
            "SELECT COUNT(*) FROM commit_plans WHERE source='manual'"
        ),
        1
    );
    assert_eq!(
        manual_link_count(
            &board,
            "SELECT COUNT(*) FROM commit_tasks WHERE source='manual'"
        ),
        1
    );
    assert_eq!(
        manual_link_count(
            &board,
            "SELECT COUNT(*) FROM commits WHERE subject='repaired by hand' AND insertions=2 AND deletions=3"
        ),
        1
    );
    assert_eq!(
        manual_link_count(
            &board,
            concat!(
                "SELECT COUNT(*) FROM entries e JOIN actors a ON a.id=e.actor_id ",
                "WHERE e.kind='commit' AND a.harness='human' AND a.session='linker'"
            )
        ),
        1,
        "the commit entry names the linker"
    );
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM plan_repos"),
        1
    );
}

#[test]
fn manual_commit_link_is_idempotent_and_returns_the_original_receipt() {
    let directory = crate::board::board_test_support::scratch("board-fixture-");
    let mut board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    let (owner, task) = manual_link_plan(&mut board);
    let oid = crate::identity::GitOid::parse(&"b".repeat(40)).unwrap();
    let request = || {
        BoardRequest::new(
            owner.clone(),
            BoardOp::LinkCommit {
                oid,
                task,
                resolution: Some(Box::new(manual_link_resolution())),
            },
        )
    };
    let first = board.handle(&request()).unwrap();
    let BoardResult::Change(first) = first.result else {
        panic!("change result")
    };
    let events = manual_link_count(&board, "SELECT COUNT(*) FROM events");
    let second = board.handle(&request()).unwrap();
    let BoardResult::Change(second) = second.result else {
        panic!("change result")
    };
    assert!(second.deduplicated);
    assert_eq!(second.entry, first.entry);
    assert_eq!(second.seq, first.seq);
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM commit_plans"),
        1
    );
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM commit_tasks"),
        1
    );
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM entries WHERE kind='commit'"),
        1
    );
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM events"),
        events,
        "an idempotent re-link mints no event"
    );
}

fn manual_link_attempt(
    board: &mut LocalBoard,
    actor: BoardActor,
    task: TaskId,
) -> Result<crate::board::board_protocol::BoardReply, crate::board::board_protocol::BoardError> {
    board.handle(&BoardRequest::new(
        actor,
        BoardOp::LinkCommit {
            oid: crate::identity::GitOid::parse(&"b".repeat(40)).unwrap(),
            task,
            resolution: Some(Box::new(manual_link_resolution())),
        },
    ))
}

#[test]
fn manual_commit_link_authority_requires_owner_hand_or_steward() {
    let directory = crate::board::board_test_support::scratch("board-fixture-");
    let mut board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    let (_owner, task) = manual_link_plan(&mut board);
    for harness in ["human", "cli", "codex"] {
        let actor = manual_link_actor("mallory", harness);
        let error = manual_link_attempt(&mut board, actor, task).unwrap_err();
        assert_eq!(
            error.code,
            crate::board::board_protocol::BoardErrorCode::InvalidActor,
            "outsider {harness}: {error}"
        );
    }
    for harness in ["human", "codex"] {
        let actor = manual_link_actor("fixture", harness);
        let reply = manual_link_attempt(&mut board, actor.clone(), task)
            .unwrap_or_else(|error| panic!("{} must link: {error}", actor.harness));
        assert!(matches!(reply.result, BoardResult::Change(_)));
    }
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM commit_plans"),
        1
    );
}

#[test]
fn non_steward_agent_cannot_link() {
    let directory = crate::board::board_test_support::scratch("board-fixture-");
    let mut board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    let (_owner, task) = manual_link_plan(&mut board);
    let error =
        manual_link_attempt(&mut board, manual_link_actor("fixture", "kimi"), task).unwrap_err();
    assert_eq!(
        error.code,
        crate::board::board_protocol::BoardErrorCode::InvalidActor,
        "{error}"
    );
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM commit_plans"),
        0
    );
}

#[test]
fn cli_harness_cannot_link() {
    let directory = crate::board::board_test_support::scratch("board-fixture-");
    let mut board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    let (_owner, task) = manual_link_plan(&mut board);
    let error =
        manual_link_attempt(&mut board, manual_link_actor("fixture", "cli"), task).unwrap_err();
    assert_eq!(
        error.code,
        crate::board::board_protocol::BoardErrorCode::InvalidActor,
        "{error}"
    );
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM commit_plans"),
        0
    );
}

#[test]
fn relinking_to_another_task_writes_an_event() {
    let directory = crate::board::board_test_support::scratch("board-fixture-");
    let mut board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    let (owner, task) = manual_link_plan(&mut board);
    board
        .handle(&BoardRequest::new(
            owner.clone(),
            BoardOp::TaskCreate {
                plan: task.plan,
                title: PlanTitle::new("follow-up").unwrap(),
                to: None,
                section: None,
            },
        ))
        .unwrap();
    let other = TaskId::new(task.plan, 2).unwrap();
    let first = manual_link_attempt(&mut board, owner.clone(), task).unwrap();
    let BoardResult::Change(first) = first.result else {
        panic!("change result")
    };
    let events = manual_link_count(&board, "SELECT COUNT(*) FROM events");
    let second = manual_link_attempt(&mut board, owner, other).unwrap();
    let BoardResult::Change(second) = second.result else {
        panic!("change result")
    };
    assert!(
        !second.deduplicated,
        "a new task link is not a replay: {second:?}"
    );
    assert_eq!(second.entry, first.entry, "the commit keeps its plan entry");
    assert_eq!(second.task, Some(other));
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM events"),
        events + 1,
        "relinking to another task writes an event"
    );
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM commit_plans"),
        1
    );
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM commit_tasks"),
        2
    );
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM entries WHERE kind='commit'"),
        1
    );
}

#[test]
fn manual_commit_link_requires_a_host_resolved_commit() {
    let directory = crate::board::board_test_support::scratch("board-fixture-");
    let mut board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    let (owner, task) = manual_link_plan(&mut board);
    let error = board
        .handle(&BoardRequest::new(
            owner,
            BoardOp::LinkCommit {
                oid: crate::identity::GitOid::parse(&"b".repeat(40)).unwrap(),
                task,
                resolution: None,
            },
        ))
        .unwrap_err();
    assert_eq!(
        error.code,
        crate::board::board_protocol::BoardErrorCode::InvalidOptions,
        "{error}"
    );
}

#[test]
fn manual_commit_link_rejects_unknown_plans_and_tasks() {
    let directory = crate::board::board_test_support::scratch("board-fixture-");
    let mut board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    let (owner, task) = manual_link_plan(&mut board);
    let oid = crate::identity::GitOid::parse(&"b".repeat(40)).unwrap();
    for target in [
        TaskId::new(PlanId::new(99).unwrap(), 1).unwrap(),
        TaskId::new(task.plan, 99).unwrap(),
    ] {
        let error = board
            .handle(&BoardRequest::new(
                owner.clone(),
                BoardOp::LinkCommit {
                    oid,
                    task: target,
                    resolution: Some(Box::new(manual_link_resolution())),
                },
            ))
            .unwrap_err();
        assert_eq!(
            error.code,
            crate::board::board_protocol::BoardErrorCode::InvalidReference,
            "{target}: {error}"
        );
    }
}

#[test]
fn scan_links_keep_scan_source_beside_manual_links() {
    let directory = crate::board::board_test_support::scratch("board-fixture-");
    let mut board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    let (owner, task) = manual_link_plan(&mut board);
    let scanned_oid = crate::identity::GitOid::parse(&"c".repeat(40)).unwrap();
    let mut scanned = manual_link_resolution();
    scanned.oid = scanned_oid;
    scanned.repo_key = RepoKey::from_roots([scanned_oid]).unwrap();
    scanned.plans = vec![CommitPlanLink {
        plan_id: task.plan,
        task_ordinal: Some(task.ordinal),
    }];
    board
        .handle(&BoardRequest::new(
            owner.clone(),
            BoardOp::LinkCommits {
                commits: vec![scanned],
            },
        ))
        .unwrap();
    assert_eq!(
        manual_link_count(
            &board,
            "SELECT COUNT(*) FROM commit_plans WHERE source='scan'"
        ),
        1
    );
    assert_eq!(
        manual_link_count(
            &board,
            "SELECT COUNT(*) FROM commit_tasks WHERE source='scan'"
        ),
        1
    );
    board
        .handle(&BoardRequest::new(
            owner,
            BoardOp::LinkCommit {
                oid: crate::identity::GitOid::parse(&"b".repeat(40)).unwrap(),
                task,
                resolution: Some(Box::new(manual_link_resolution())),
            },
        ))
        .unwrap();
    assert_eq!(
        manual_link_count(
            &board,
            "SELECT COUNT(*) FROM commit_plans WHERE source='manual'"
        ),
        1
    );
}
