use super::*;

#[test]
fn commit_tasks_retain_all_valid_links_and_unknown_task_falls_back() {
    let directory = crate::board::board_test_support::scratch("board-fixture-");
    let mut board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    let actor = BoardActor::new(
        "fixture",
        "fixture-host",
        HarnessLabel::parse("human").unwrap(),
        "commit-links",
    )
    .unwrap();
    let reply = board
        .handle(&BoardRequest::new(
            actor.clone(),
            BoardOp::New {
                title: PlanTitle::new("Commit links").unwrap(),
                body: PlanText::new("Commit links").unwrap(),
                steward: None,
                repo_key: None,
            },
        ))
        .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("plan result")
    };
    let plan = change.plan.unwrap();
    for title in ["first", "second"] {
        board
            .handle(&BoardRequest::new(
                actor.clone(),
                BoardOp::TaskCreate {
                    plan,
                    title: PlanTitle::new(title).unwrap(),
                    to: None,
                    section: None,
                },
            ))
            .unwrap();
    }
    let oid = crate::identity::GitOid::parse(&"a".repeat(40)).unwrap();
    let key = RepoKey::from_roots([oid]).unwrap();
    let commit = LinkedCommit {
        repo_key: key,
        oid,
        subject: "two tasks and unknown".into(),
        committed_at: 1,
        author: "Fixture <fixture@example.test>".into(),
        coauthors: Vec::new(),
        files: 0,
        insertions: 0,
        deletions: 0,
        plans: vec![
            CommitPlanLink {
                plan_id: plan,
                task_ordinal: Some(1),
            },
            CommitPlanLink {
                plan_id: plan,
                task_ordinal: Some(2),
            },
            CommitPlanLink {
                plan_id: plan,
                task_ordinal: Some(99),
            },
        ],
    };
    let request = BoardRequest::new(
        actor,
        BoardOp::LinkCommits {
            commits: vec![commit],
        },
    );
    let reply = board.handle(&request).unwrap();
    let json = serde_json::to_value(&reply.result).unwrap();
    assert_eq!(
        json["data"]["unknown_tasks"],
        serde_json::json!([TaskId::new(plan, 99).unwrap()])
    );
    assert_eq!(
        board
            .conn
            .query_row("SELECT COUNT(*) FROM commit_plans", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        board
            .conn
            .query_row("SELECT COUNT(*) FROM commit_tasks", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        board
            .conn
            .query_row(
                "SELECT COUNT(*) FROM entries WHERE kind='commit'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert!(matches!(
        board.handle(&request).unwrap().result,
        BoardResult::CommitsLinked(CommitLinkResult { inserted: 0, .. })
    ));
}
