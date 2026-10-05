use super::*;

#[test]
fn common_directory_rekeys_when_unrelated_repository_replaces_content() {
    let fixture = crate::board::repo_identity::tests::GitFixture::new();
    fixture.commit("original root");
    let original = fixture.registration();
    let path = fixture.directory.path().join("board.sqlite3");
    let mut board = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    let actor = BoardActor::new(
        "fixture",
        "fixture-host",
        HarnessLabel::parse("human").unwrap(),
        "repo-rekey",
    )
    .unwrap();
    let reply = board
        .handle(&BoardRequest::new(
            actor.clone(),
            BoardOp::New {
                title: PlanTitle::new("Rekey").unwrap(),
                body: PlanText::new("Rekey").unwrap(),
                steward: None,
                repo_key: None,
            },
        ))
        .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("plan result")
    };
    let plan = change.plan.unwrap();
    board
        .handle(&BoardRequest::new(
            actor.clone(),
            BoardOp::RegisterRepo {
                registration: original.clone(),
            },
        ))
        .unwrap();
    let link = board
        .handle(&BoardRequest::new(
            actor.clone(),
            BoardOp::LinkCommits {
                commits: vec![LinkedCommit {
                    repo_key: original.repo_key.clone(),
                    oid: original.root_commits[0],
                    subject: "original root".into(),
                    committed_at: 1,
                    author: "Fixture <fixture@example.test>".into(),
                    coauthors: Vec::new(),
                    files: 0,
                    insertions: 0,
                    deletions: 0,
                    plans: vec![CommitPlanLink {
                        plan_id: plan,
                        task_ordinal: None,
                    }],
                }],
            },
        ))
        .unwrap();
    assert!(matches!(
        link.result,
        BoardResult::CommitsLinked(CommitLinkResult { inserted: 1, .. })
    ));
    std::fs::remove_dir_all(fixture.root.join(".git")).unwrap();
    fixture.git(&["init", "--quiet", "--initial-branch=main"]);
    fixture.commit("unrelated root");
    let replacement = fixture.registration();
    assert_ne!(replacement.repo_key, original.repo_key);
    assert_eq!(replacement.common_dir, original.common_dir);
    let reply = board
        .handle(&BoardRequest::new(
            actor,
            BoardOp::RegisterRepo {
                registration: replacement.clone(),
            },
        ))
        .unwrap();
    let BoardResult::Registered(registration) = reply.result else {
        panic!("registration result")
    };
    assert_eq!(registration.repo_key, replacement.repo_key);
    assert_eq!(registration.root_commits, replacement.root_commits);
    assert_eq!(
        board
            .conn
            .query_row(
                "SELECT repo_key FROM repo_paths WHERE host='fixture-host'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        replacement.repo_key.as_str()
    );
    assert_eq!(
        board
            .conn
            .query_row(
                "SELECT COUNT(*) FROM commits WHERE repo_key=?1",
                [original.repo_key.as_str()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        board
            .conn
            .query_row(
                "SELECT COUNT(*) FROM commit_plans WHERE repo_key=?1",
                [original.repo_key.as_str()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        board
            .conn
            .query_row(
                "SELECT COUNT(*) FROM plan_repos WHERE repo_key=?1",
                [replacement.repo_key.as_str()],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn disjoint_roots_rekey_starts_without_copied_plan_links() {
    let first = crate::board::repo_identity::tests::GitFixture::new();
    first.commit("first root");
    let second = crate::board::repo_identity::tests::GitFixture::new();
    second.commit("second root");
    let mut original = first.registration();
    let mut replaced = second.registration();
    replaced.common_dir = original.common_dir.clone();
    assert_ne!(original.repo_key, replaced.repo_key);
    assert!(crate::board::repo_identity::root_sets_diverged(
        &original.root_commits,
        &replaced.root_commits
    ));
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
        "repo-rekey",
    )
    .unwrap();
    let reply = board
        .handle(&BoardRequest::new(
            actor.clone(),
            BoardOp::New {
                title: PlanTitle::new("Rekeyed links").unwrap(),
                body: PlanText::new("Rekeyed links").unwrap(),
                steward: None,
                repo_key: None,
            },
        ))
        .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("plan result")
    };
    original.plan_id = Some(change.plan.unwrap());
    board
        .handle(&BoardRequest::new(
            actor.clone(),
            BoardOp::RegisterRepo {
                registration: original.clone(),
            },
        ))
        .unwrap();
    let reply = board
        .handle(&BoardRequest::new(
            actor,
            BoardOp::RegisterRepo {
                registration: replaced.clone(),
            },
        ))
        .unwrap();
    let BoardResult::Registered(rekeyed) = reply.result else {
        panic!("registration result")
    };
    assert_eq!(rekeyed.repo_key, replaced.repo_key);
    let links = |key: &RepoKey| -> i64 {
        board
            .conn
            .query_row(
                "SELECT COUNT(*) FROM plan_repos WHERE repo_key=?1",
                params![key.as_str()],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(links(&replaced.repo_key), 0);
    assert_eq!(links(&original.repo_key), 1);
}
