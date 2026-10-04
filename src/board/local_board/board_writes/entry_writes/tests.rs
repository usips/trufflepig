use super::*;
use crate::board::board_ids::TaskId;
use crate::board::board_vocabulary::{PlanText, PlanTitle};

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
}

#[test]
fn common_directory_keeps_identity_when_fetch_extends_root_set() {
    let fixture = crate::board::repo_identity::tests::GitFixture::new();
    fixture.commit("original root");
    let original = fixture.registration();
    let foreign = crate::board::repo_identity::tests::GitFixture::new();
    foreign.commit("foreign root");
    let path = fixture.directory.path().join("board.sqlite3");
    let mut board = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    let actor = BoardActor::new(
        "fixture",
        "fixture-host",
        HarnessLabel::parse("human").unwrap(),
        "repo-fetch",
    )
    .unwrap();
    board
        .handle(&BoardRequest::new(
            actor.clone(),
            BoardOp::RegisterRepo {
                registration: original.clone(),
            },
        ))
        .unwrap();
    fixture.git(&[
        "fetch",
        "--quiet",
        foreign.root.to_str().unwrap(),
        "main:refs/remotes/foreign/main",
    ]);
    let extended = fixture.registration();
    assert_eq!(extended.root_commits.len(), 2);
    assert_ne!(extended.repo_key, original.repo_key);
    let reply = board
        .handle(&BoardRequest::new(
            actor,
            BoardOp::RegisterRepo {
                registration: extended,
            },
        ))
        .unwrap();
    let BoardResult::Registered(registration) = reply.result else {
        panic!("registration result")
    };
    assert_eq!(registration.repo_key, original.repo_key);
}

#[test]
fn common_directory_keeps_first_identity_when_root_set_expands() {
    let fixture = crate::board::repo_identity::tests::GitFixture::new();
    fixture.commit("original root");
    let first = fixture.registration();
    let path = fixture.directory.path().join("board.sqlite3");
    let mut board = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    let actor = BoardActor::new(
        "fixture",
        "fixture-host",
        HarnessLabel::parse("human").unwrap(),
        "repo-roots",
    )
    .unwrap();
    board
        .handle(&BoardRequest::new(
            actor.clone(),
            BoardOp::RegisterRepo {
                registration: first.clone(),
            },
        ))
        .unwrap();
    fixture.git(&["switch", "--orphan", "new-root"]);
    fixture.commit("additional root");
    let orphan = fixture.registration();
    let reply = board
        .handle(&BoardRequest::new(
            actor.clone(),
            BoardOp::RegisterRepo {
                registration: orphan,
            },
        ))
        .unwrap();
    let BoardResult::Registered(orphan) = reply.result else {
        panic!("registration result")
    };
    assert_eq!(orphan.repo_key, first.repo_key);
    fixture.git(&["merge", "--allow-unrelated-histories", "--no-edit", "main"]);
    let expanded = fixture.registration();
    assert_ne!(expanded.repo_key, first.repo_key);
    let reply = board
        .handle(&BoardRequest::new(
            actor.clone(),
            BoardOp::RegisterRepo {
                registration: expanded,
            },
        ))
        .unwrap();
    let BoardResult::Registered(registration) = reply.result else {
        panic!("registration result")
    };
    assert_eq!(registration.repo_key, first.repo_key);
    let mut conflicting = fixture.registration();
    let configured = conflicting.repo_key.clone();
    conflicting.origin_override = Some(configured.clone());
    let error = board
        .handle(&BoardRequest::new(
            actor.clone(),
            BoardOp::RegisterRepo {
                registration: conflicting,
            },
        ))
        .unwrap_err();
    assert!(error.message.contains(configured.as_str()));
    assert!(error.message.contains(first.repo_key.as_str()));
    fixture.git(&["switch", "main"]);
    let restored = fixture.registration();
    let reply = board
        .handle(&BoardRequest::new(
            actor,
            BoardOp::RegisterRepo {
                registration: restored,
            },
        ))
        .unwrap();
    let BoardResult::Registered(restored) = reply.result else {
        panic!("registration result")
    };
    assert_eq!(restored.repo_key, first.repo_key);
    assert_eq!(restored.root_commits, first.root_commits);
    assert_eq!(
        board
            .conn
            .query_row("SELECT COUNT(*) FROM repo_paths", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
