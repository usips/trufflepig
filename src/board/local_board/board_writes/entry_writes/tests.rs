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
fn common_directory_keeps_plan_links_when_root_set_extends() {
    let fixture = crate::board::repo_identity::tests::GitFixture::new();
    fixture.commit("original root");
    let original = fixture.registration();
    let path = fixture.directory.path().join("board.sqlite3");
    let mut board = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    let actor = BoardActor::new(
        "fixture",
        "fixture-host",
        HarnessLabel::parse("human").unwrap(),
        "repo-extend",
    )
    .unwrap();
    let reply = board
        .handle(&BoardRequest::new(
            actor.clone(),
            BoardOp::New {
                title: PlanTitle::new("Extend").unwrap(),
                body: PlanText::new("Extend").unwrap(),
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
    fixture.git(&["switch", "--orphan", "new-root"]);
    fixture.commit("additional root");
    let extended = fixture.registration();
    assert_eq!(extended.root_commits.len(), 2);
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
    assert_eq!(
        board
            .conn
            .query_row(
                "SELECT COUNT(*) FROM plan_repos WHERE repo_key=?1",
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

fn manual_link_actor(user: &str, harness: &str) -> BoardActor {
    BoardActor::new(user, "fixture-host", HarnessLabel::parse(harness).unwrap(), "linker").unwrap()
}

fn manual_link_plan(board: &mut LocalBoard) -> (BoardActor, TaskId) {
    let owner = manual_link_actor("fixture", "human");
    let reply = board
        .handle(&BoardRequest::new(
            owner.clone(),
            BoardOp::New {
                title: PlanTitle::new("Manual links").unwrap(),
                body: PlanText::new("Manual links").unwrap(),
                steward: Some(HarnessLabel::parse("codex").unwrap()),
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
            owner.clone(),
            BoardOp::TaskCreate {
                plan,
                title: PlanTitle::new("repair").unwrap(),
                to: None,
                section: None,
            },
        ))
        .unwrap();
    (owner, TaskId::new(plan, 1).unwrap())
}

fn manual_link_resolution() -> LinkedCommit {
    let oid = crate::identity::GitOid::parse(&"b".repeat(40)).unwrap();
    LinkedCommit {
        repo_key: RepoKey::from_roots([oid]).unwrap(),
        oid,
        subject: "repaired by hand".into(),
        committed_at: 12,
        author: "Fixture <fixture@example.test>".into(),
        coauthors: Vec::new(),
        files: 1,
        insertions: 2,
        deletions: 3,
        plans: Vec::new(),
    }
}

fn manual_link_count(board: &LocalBoard, sql: &str) -> i64 {
    board
        .conn
        .query_row(sql, [], |row| row.get::<_, i64>(0))
        .unwrap()
}

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
        manual_link_count(&board, "SELECT COUNT(*) FROM commit_plans WHERE source='manual'"),
        1
    );
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM commit_tasks WHERE source='manual'"),
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
    assert_eq!(manual_link_count(&board, "SELECT COUNT(*) FROM commit_plans"), 1);
    assert_eq!(manual_link_count(&board, "SELECT COUNT(*) FROM commit_tasks"), 1);
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

#[test]
fn manual_commit_link_authority_covers_steward_owner_and_humans() {
    let directory = crate::board::board_test_support::scratch("board-fixture-");
    let mut board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    let (_owner, task) = manual_link_plan(&mut board);
    let oid = crate::identity::GitOid::parse(&"b".repeat(40)).unwrap();
    let outsider = manual_link_actor("mallory", "kimi");
    let error = board
        .handle(&BoardRequest::new(
            outsider,
            BoardOp::LinkCommit {
                oid,
                task,
                resolution: Some(Box::new(manual_link_resolution())),
            },
        ))
        .unwrap_err();
    assert_eq!(
        error.code,
        crate::board::board_protocol::BoardErrorCode::InvalidActor,
        "{error}"
    );
    for actor in [
        manual_link_actor("mallory", "human"),
        manual_link_actor("mallory", "cli"),
        manual_link_actor("mallory", "codex"),
        manual_link_actor("fixture", "kimi"),
    ] {
        let reply = board
            .handle(&BoardRequest::new(
                actor.clone(),
                BoardOp::LinkCommit {
                    oid,
                    task,
                    resolution: Some(Box::new(manual_link_resolution())),
                },
            ))
            .unwrap_or_else(|error| panic!("{} must link: {error}", actor.harness));
        assert!(matches!(reply.result, BoardResult::Change(_)));
    }
    assert_eq!(manual_link_count(&board, "SELECT COUNT(*) FROM commit_plans"), 1);
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
        manual_link_count(&board, "SELECT COUNT(*) FROM commit_plans WHERE source='scan'"),
        1
    );
    assert_eq!(
        manual_link_count(&board, "SELECT COUNT(*) FROM commit_tasks WHERE source='scan'"),
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
        manual_link_count(&board, "SELECT COUNT(*) FROM commit_plans WHERE source='manual'"),
        1
    );
}
