use super::*;
use crate::board::board_actor::HarnessLabel;
use crate::board::board_ids::{PlanId, TaskId};
use crate::board::board_protocol::{
    BoardOp, BoardRequest, BoardResult, ClaimResume, CommitCoauthor, CommitLinkResult,
    CommitPlanLink, LinkedCommit, WorkerAddress,
};
use crate::board::board_vocabulary::{EntryText, PlanText, PlanTitle};
use crate::board::local_board::LocalBoard;
use crate::identity::GitOid;
use rusqlite::params;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn owner() -> BoardActor {
    BoardActor::new(
        "fixture",
        "fixture-host",
        HarnessLabel::parse("human").unwrap(),
        "commit-test-owner",
    )
    .unwrap()
}

fn create_plan(board: &mut LocalBoard, title: &str) -> PlanId {
    let reply = board
        .handle(&BoardRequest::new(
            owner(),
            BoardOp::New {
                title: PlanTitle::new(title).unwrap(),
                body: PlanText::new(title).unwrap(),
                repo_key: None,
            },
        ))
        .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("plan creation failed")
    };
    change.plan.unwrap()
}

fn add_task(
    board: &mut LocalBoard,
    plan: PlanId,
    ordinal: u64,
    claim_harness: &str,
) -> (TaskId, i64) {
    let task = TaskId::new(plan, ordinal).unwrap();
    board
        .handle(&BoardRequest::new(
            owner(),
            BoardOp::TaskCreate {
                plan,
                title: PlanTitle::new(format!("task {task}")).unwrap(),
                to: None,
                section: None,
            },
        ))
        .unwrap();
    let claimant = BoardActor::new(
        "fixture",
        "fixture-host",
        HarnessLabel::parse(claim_harness).unwrap(),
        format!("claim-{task}"),
    )
    .unwrap();
    board
        .handle(&BoardRequest::new(
            claimant,
            BoardOp::ClaimTask {
                task,
                scope: Some(EntryText::new(format!("scope {task}")).unwrap()),
                resume: ClaimResume::No,
                delegate: None,
            },
        ))
        .unwrap();
    let entry: i64 = board
        .conn
        .query_row(
            "SELECT entry_id FROM claims WHERE plan_id=?1 AND task_ordinal=?2",
            params![plan.get() as i64, ordinal as i64],
            |row| row.get(0),
        )
        .unwrap();
    (task, entry)
}

fn set_claim_window(board: &LocalBoard, task: TaskId, start: i64, end: Option<i64>) {
    board
        .conn
        .execute(
            "UPDATE claims SET claimed_at=?3,ended_at=?4 WHERE plan_id=?1 AND task_ordinal=?2",
            params![task.plan.get() as i64, task.ordinal as i64, start, end],
        )
        .unwrap();
}

fn coauthor(harness: &str, model: &str) -> CommitCoauthor {
    let (harness, email) = match harness {
        "codex" => ("codex", "agent@openai.com"),
        "claude" => ("claude", "agent@anthropic.com"),
        _ => panic!("test harness must name a known vendor"),
    };
    CommitCoauthor {
        harness: HarnessLabel::parse(harness).unwrap(),
        model: model.to_owned(),
        email: email.to_owned(),
    }
}

fn linked_commit(
    oid_letter: char,
    committed_at: i64,
    tasks: &[TaskId],
    coauthors: Vec<CommitCoauthor>,
    worker_refs: Vec<WorkerAddress>,
) -> LinkedCommit {
    let oid = GitOid::parse(&oid_letter.to_string().repeat(40)).unwrap();
    let root = GitOid::parse(&"f".repeat(40)).unwrap();
    LinkedCommit {
        repo_key: crate::board::board_ids::RepoKey::from_roots([root]).unwrap(),
        oid,
        subject: format!("Commit subject {oid_letter}"),
        committed_at,
        author: "Fixture <fixture@example.test>".into(),
        coauthors,
        files: 1,
        insertions: 2,
        deletions: 0,
        plans: tasks
            .iter()
            .map(|task| CommitPlanLink {
                plan_id: task.plan,
                task_ordinal: Some(task.ordinal),
            })
            .collect(),
        worker_refs,
        claim_links: Vec::new(),
    }
}

fn scan(board: &mut LocalBoard, commit: LinkedCommit) -> CommitLinkResult {
    let actor = BoardActor::system("fixture", "fixture-host").unwrap();
    let request = BoardRequest::system(
        actor,
        BoardOp::LinkCommits {
            commits: vec![commit],
        },
    )
    .with_system_origin("ingest")
    .unwrap();
    let reply = board.handle(&request).unwrap();
    let BoardResult::CommitsLinked(result) = reply.result else {
        panic!("commit result: {reply:?}")
    };
    result
}

fn claim_for(board: &LocalBoard, oid: GitOid, task: TaskId) -> Option<i64> {
    board
        .conn
        .query_row(
            "SELECT claim_entry FROM commit_tasks WHERE oid=?1 AND plan_id=?2 AND task_ordinal=?3",
            params![oid.as_str(), task.plan.get() as i64, task.ordinal as i64],
            |row| row.get(0),
        )
        .unwrap()
}

fn unix_now() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
}

#[test]
fn scan_event_keeps_multiplan_task_model_and_claim_evidence_and_is_idempotent() {
    let directory = crate::test_fixtures::scratch("board-commit-attribution-");
    let mut board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    let first_plan = create_plan(&mut board, "First plan");
    let (first_task, first_claim) = add_task(&mut board, first_plan, 1, "codex");
    let (second_task, second_claim) = add_task(&mut board, first_plan, 2, "codex");
    let second_plan = create_plan(&mut board, "Second plan");
    let (third_task, third_claim) = add_task(&mut board, second_plan, 1, "codex");
    for task in [first_task, second_task, third_task] {
        set_claim_window(&board, task, 1000, Some(2000));
    }
    let unknown_worker = WorkerAddress::parse(&format!("{first_task}/missing-handle")).unwrap();
    let commit = linked_commit(
        'a',
        1500,
        &[first_task, second_task, third_task],
        vec![
            coauthor("codex", "gpt-6.1-sol"),
            coauthor("claude", "claude-4"),
        ],
        vec![unknown_worker.clone()],
    );
    let linked = scan(&mut board, commit.clone());
    assert_eq!(linked.unknown_workers, vec![unknown_worker]);
    assert_eq!(linked.inserted, 2);
    assert_eq!(claim_for(&board, commit.oid, first_task), Some(first_claim));
    assert_eq!(
        claim_for(&board, commit.oid, second_task),
        Some(second_claim)
    );
    assert_eq!(claim_for(&board, commit.oid, third_task), Some(third_claim));
    let (summary, harness, origin_harness, origin_session): (String, String, String, String) =
        board
            .conn
            .query_row(
                concat!(
                    "SELECT e.summary,a.harness,e.origin_harness,e.origin_session FROM events e ",
                    "JOIN actors a ON a.id=e.actor_id WHERE e.kind='commit' ORDER BY e.seq DESC LIMIT 1"
                ),
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
    assert_eq!(
        (
            harness.as_str(),
            origin_harness.as_str(),
            origin_session.as_str()
        ),
        ("system", "system", "ingest")
    );
    for expected in [
        commit.oid.to_string(),
        commit.oid.as_str()[..8].to_owned(),
        commit.subject.clone(),
        first_task.to_string(),
        second_task.to_string(),
        third_task.to_string(),
        "gpt-6.1-sol".to_owned(),
        "claude-4".to_owned(),
        format!("E{first_claim}"),
        format!("E{second_claim}"),
        format!("E{third_claim}"),
    ] {
        assert!(
            summary.contains(&expected),
            "missing {expected:?} in {summary:?}"
        );
    }
    let coauthors: String = board
        .conn
        .query_row(
            "SELECT coauthors FROM commits WHERE oid=?1",
            [commit.oid.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Vec<CommitCoauthor>>(&coauthors).unwrap(),
        commit.coauthors
    );
    let event_count: i64 = board
        .conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE kind='commit'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(event_count, 1);

    assert_eq!(scan(&mut board, commit).inserted, 0);
    let next = scan(
        &mut board,
        linked_commit(
            'b',
            1600,
            &[first_task],
            vec![coauthor("codex", "gpt-6.1-sol")],
            Vec::new(),
        ),
    );
    assert_eq!(next.inserted, 1, "each commit has its own plan link");
    let (count, distinct): (i64, i64) = board
        .conn
        .query_row(
            "SELECT COUNT(*),COUNT(DISTINCT seq) FROM events WHERE kind='commit'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((count, distinct), (2, 2));
}

#[test]
fn claim_attribution_requires_vendor_and_strict_observed_time_window() {
    let directory = crate::test_fixtures::scratch("board-commit-window-");
    let mut board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    let plan = create_plan(&mut board, "Claim window");
    let (task, claim) = add_task(&mut board, plan, 1, "codex");
    set_claim_window(&board, task, 1000, Some(2000));
    let future = unix_now() + 3600;
    let cases = [
        ('a', 1500, "codex", Some(claim)),
        ('b', 1600, "claude", None),
        ('c', 1000, "codex", None),
        ('d', 2000, "codex", None),
        ('e', 2001, "codex", None),
        ('f', future, "codex", None),
    ];
    for (oid, at, harness, expected) in cases {
        let commit = linked_commit(
            oid,
            at,
            &[task],
            vec![coauthor(
                harness,
                if harness == "codex" {
                    "gpt-6.1-sol"
                } else {
                    "claude-4"
                },
            )],
            Vec::new(),
        );
        scan(&mut board, commit.clone());
        assert_eq!(
            claim_for(&board, commit.oid, task),
            expected,
            "{oid} at {at}"
        );
    }

    let open_plan = create_plan(&mut board, "Open claim");
    let (open_task, open_claim) = add_task(&mut board, open_plan, 1, "codex");
    set_claim_window(&board, open_task, 1000, None);
    let recent = unix_now() - 1;
    let open_commit = linked_commit(
        '7',
        recent,
        &[open_task],
        vec![coauthor("codex", "gpt-6.1-sol")],
        Vec::new(),
    );
    scan(&mut board, open_commit.clone());
    assert_eq!(
        claim_for(&board, open_commit.oid, open_task),
        Some(open_claim)
    );
}

#[test]
fn takeover_intervals_keep_commits_with_the_original_claim_and_leave_boundary_unassigned() {
    let directory = crate::test_fixtures::scratch("board-commit-takeover-");
    let mut board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    let plan = create_plan(&mut board, "Takeover");
    let (task, old_claim) = add_task(&mut board, plan, 1, "codex");
    set_claim_window(&board, task, 1000, Some(1500));
    let original = BoardActor::new(
        "fixture",
        "fixture-host",
        HarnessLabel::parse("codex").unwrap(),
        format!("claim-{task}"),
    )
    .unwrap();
    board
        .handle(&BoardRequest::new(
            original,
            BoardOp::TaskMove {
                task,
                column: crate::board::board_vocabulary::TaskColumn::Todo,
                to: None,
            },
        ))
        .unwrap();
    let takeover = BoardActor::new(
        "fixture",
        "fixture-host",
        HarnessLabel::parse("claude").unwrap(),
        "takeover-claude",
    )
    .unwrap();
    board
        .handle(&BoardRequest::new(
            takeover,
            BoardOp::ClaimTask {
                task,
                scope: Some(EntryText::new("replacement claim").unwrap()),
                resume: ClaimResume::No,
                delegate: None,
            },
        ))
        .unwrap();
    let new_claim: i64 = board
        .conn
        .query_row(
            "SELECT entry_id FROM claims WHERE plan_id=?1 AND task_ordinal=?2 ORDER BY id DESC LIMIT 1",
            params![task.plan.get() as i64, task.ordinal as i64],
            |row| row.get(0),
        )
        .unwrap();
    board
        .conn
        .execute(
            "UPDATE claims SET claimed_at=1500,ended_at=2500 WHERE entry_id=?1",
            [new_claim],
        )
        .unwrap();
    board
        .conn
        .execute(
            "UPDATE claims SET ended_at=1500 WHERE entry_id=?1",
            [old_claim],
        )
        .unwrap();

    for (oid, at, harness, expected) in [
        ('a', 1200, "codex", Some(old_claim)),
        ('b', 2000, "claude", Some(new_claim)),
        ('c', 1500, "codex", None),
    ] {
        let commit = linked_commit(
            oid,
            at,
            &[task],
            vec![coauthor(
                harness,
                if harness == "codex" {
                    "gpt-6.1-sol"
                } else {
                    "claude-4"
                },
            )],
            Vec::new(),
        );
        scan(&mut board, commit.clone());
        assert_eq!(claim_for(&board, commit.oid, task), expected);
    }
}

#[test]
fn overlapping_claim_windows_stay_unassigned_even_when_only_one_vendor_matches() {
    let directory = crate::test_fixtures::scratch("board-commit-overlap-");
    let mut board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    let plan = create_plan(&mut board, "Overlapping claims");
    let (task, codex_claim) = add_task(&mut board, plan, 1, "codex");
    set_claim_window(&board, task, 1000, Some(2500));
    let original = BoardActor::new(
        "fixture",
        "fixture-host",
        HarnessLabel::parse("codex").unwrap(),
        format!("claim-{task}"),
    )
    .unwrap();
    board
        .handle(&BoardRequest::new(
            original,
            BoardOp::TaskMove {
                task,
                column: crate::board::board_vocabulary::TaskColumn::Todo,
                to: None,
            },
        ))
        .unwrap();
    let takeover = BoardActor::new(
        "fixture",
        "fixture-host",
        HarnessLabel::parse("claude").unwrap(),
        "overlapping-claude",
    )
    .unwrap();
    board
        .handle(&BoardRequest::new(
            takeover,
            BoardOp::ClaimTask {
                task,
                scope: Some(EntryText::new("overlapping claim").unwrap()),
                resume: ClaimResume::No,
                delegate: None,
            },
        ))
        .unwrap();
    let claude_claim: i64 = board
        .conn
        .query_row(
            "SELECT entry_id FROM claims WHERE plan_id=?1 AND task_ordinal=?2 ORDER BY id DESC LIMIT 1",
            params![task.plan.get() as i64, task.ordinal as i64],
            |row| row.get(0),
        )
        .unwrap();
    board
        .conn
        .execute(
            "UPDATE claims SET claimed_at=1000,ended_at=2500 WHERE entry_id=?1",
            [codex_claim],
        )
        .unwrap();
    board
        .conn
        .execute(
            "UPDATE claims SET claimed_at=1500,ended_at=3000 WHERE entry_id=?1",
            [claude_claim],
        )
        .unwrap();
    let commit = linked_commit(
        '8',
        2000,
        &[task],
        vec![coauthor("claude", "claude-4")],
        Vec::new(),
    );
    scan(&mut board, commit.clone());
    assert_eq!(claim_for(&board, commit.oid, task), None);
}

#[test]
fn scan_refreshes_matching_claim_activity_at_commit_time() {
    let directory = crate::test_fixtures::scratch("board-commit-activity-");
    let mut board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    let plan = create_plan(&mut board, "Commit activity");
    let (task, _claim) = add_task(&mut board, plan, 1, "codex");
    set_claim_window(&board, task, 1000, None);
    board
        .conn
        .execute(
            "UPDATE claims SET last_active=1000 WHERE plan_id=?1 AND task_ordinal=?2",
            params![task.plan.get() as i64, task.ordinal as i64],
        )
        .unwrap();
    let commit = linked_commit(
        'c',
        1500,
        &[task],
        vec![coauthor("codex", "gpt-6.1-sol")],
        Vec::new(),
    );
    assert_eq!(scan(&mut board, commit).inserted, 1);
    let last_active: i64 = board
        .conn
        .query_row(
            "SELECT last_active FROM claims WHERE plan_id=?1 AND task_ordinal=?2",
            params![task.plan.get() as i64, task.ordinal as i64],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(last_active, 1500);
}
