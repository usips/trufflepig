use super::*;
use crate::board::board_backend::BoardBackend;
use crate::board::board_ids::{RevisionSpan, TaskId};
use crate::board::board_vocabulary::{FeedbackKind, TaskColumn};
use crate::board::local_board::LocalBoard;
use std::time::Duration;

fn database() -> (tempfile::TempDir, LocalBoard) {
    let parent = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/board-read-tests");
    std::fs::create_dir_all(&parent).unwrap();
    let directory = tempfile::Builder::new().tempdir_in(parent).unwrap();
    let board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(7200),
    )
    .unwrap();
    (directory, board)
}

fn call(board: &mut LocalBoard, harness: &str, op: BoardOp) -> BoardResult {
    let actor = BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse(harness).unwrap(),
        "session1",
    )
    .unwrap();
    board.handle(&BoardRequest::new(actor, op)).unwrap().result
}

fn new_plan(board: &mut LocalBoard) -> PlanId {
    match call(
        board,
        "human",
        BoardOp::New {
            title: PlanTitle::new("Trial").unwrap(),
            body: PlanText::new("# Covered\n# Uncovered\n```\n# Not a section\n```\n").unwrap(),
            steward: None,
        },
    ) {
        BoardResult::Change(change) => change.plan.unwrap(),
        other => panic!("unexpected {other:?}"),
    }
}

fn post(
    board: &mut LocalBoard,
    harness: &str,
    plan: PlanId,
    kind: EntryKind,
    body: &str,
) -> EntryId {
    match call(
        board,
        harness,
        BoardOp::Post {
            target: BoardRef::Plan(plan),
            kind,
            body: EntryText::new(body).unwrap(),
            to: None,
            supersedes: None,
        },
    ) {
        BoardResult::Change(change) => change.entry,
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn plan_views_keep_old_open_questions_and_resolve_references_to_answers() {
    let (_directory, mut board) = database();
    let plan = new_plan(&mut board);
    let question = post(
        &mut board,
        "claude",
        plan,
        EntryKind::Question,
        "Should this work?",
    );
    for index in 0..25 {
        post(
            &mut board,
            "codex",
            plan,
            EntryKind::Progress,
            &format!("update {index}"),
        );
    }
    call(
        &mut board,
        "human",
        BoardOp::TaskCreate {
            plan,
            title: PlanTitle::new("Covered task").unwrap(),
            to: None,
            section: Some("Covered".to_owned()),
        },
    );
    let BoardResult::Plan(view) = call(
        &mut board,
        "codex",
        BoardOp::Show {
            target: Some(BoardRef::Plan(plan)),
        },
    ) else {
        panic!("missing plan")
    };
    assert_eq!(view.entries.len(), 21);
    assert!(view.entries.iter().any(|entry| entry.id == question));
    assert_eq!(view.sections_without_tasks, ["Uncovered"]);
    let answer = post(
        &mut board,
        "codex",
        plan,
        EntryKind::Answer,
        &format!("Yes: {question}"),
    );
    assert_eq!(
        entry(&board.conn, answer).unwrap().refs,
        [BoardRef::Entry(question)]
    );
    let BoardResult::Plan(view) = call(
        &mut board,
        "codex",
        BoardOp::Show {
            target: Some(BoardRef::Plan(plan)),
        },
    ) else {
        panic!("missing plan")
    };
    assert_eq!(view.entries.len(), 20);
    assert!(!view.entries.iter().any(|entry| entry.id == question));
}

#[test]
fn show_revisions_and_ranges_preserves_ssot_and_proposal_state() {
    let (_directory, mut board) = database();
    let plan = new_plan(&mut board);
    let base = PlanRevision::new(plan, 1).unwrap();
    let proposal = match call(
        &mut board,
        "codex",
        BoardOp::Propose {
            supersedes: None,
            base,
            body: PlanText::new("# Updated").unwrap(),
            summary: EntryText::new("Clarify scope").unwrap(),
        },
    ) {
        BoardResult::Change(change) => change.entry,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(
        entry(&board.conn, proposal).unwrap().state,
        Some(EntryState::Proposal(ProposalState::Open))
    );
    call(
        &mut board,
        "human",
        BoardOp::Accept {
            proposal,
            note: None,
        },
    );
    assert_eq!(
        entry(&board.conn, proposal).unwrap().state,
        Some(EntryState::Proposal(ProposalState::Accepted))
    );
    let BoardResult::Diff(diff) = call(
        &mut board,
        "codex",
        BoardOp::Show {
            target: Some(BoardRef::Span(RevisionSpan {
                plan,
                start: 1,
                end: None,
            })),
        },
    ) else {
        panic!("missing diff")
    };
    assert_eq!(diff.before.id, base);
    assert_eq!(diff.after.id.revision, 2);
    assert!(diff.before.body.as_str().contains("Uncovered"));
    assert_eq!(diff.after.body.as_str(), "# Updated");
    let BoardResult::Revision(first) = call(
        &mut board,
        "codex",
        BoardOp::Show {
            target: Some(BoardRef::Revision(base)),
        },
    ) else {
        panic!("missing revision")
    };
    assert_eq!(first, diff.before);
}

#[test]
fn review_preserves_overlapping_ended_claims_and_other_agents_open_work() {
    let (_directory, mut board) = database();
    let plan = new_plan(&mut board);
    for ordinal in 1..=2 {
        call(
            &mut board,
            "human",
            BoardOp::TaskCreate {
                plan,
                title: PlanTitle::new(format!("Lane {ordinal}")).unwrap(),
                to: None,
                section: None,
            },
        );
        let task = TaskId::new(plan, ordinal).unwrap();
        call(
            &mut board,
            "claude",
            BoardOp::ClaimTask {
                task,
                scope: Some(EntryText::new(format!("scope {ordinal}")).unwrap()),
                resume: false,
            },
        );
        call(
            &mut board,
            "claude",
            BoardOp::TaskMove {
                task,
                column: TaskColumn::Review,
                to: None,
            },
        );
    }
    board.conn.execute("UPDATE entries SET created_at=120 WHERE id=(SELECT entry_id FROM revisions WHERE plan_id=1 AND number=1)", []).unwrap();
    board
        .conn
        .execute(
            "UPDATE claims SET claimed_at=110,ended_at=130 WHERE task_ordinal=1",
            [],
        )
        .unwrap();
    board
        .conn
        .execute(
            "UPDATE claims SET claimed_at=100,ended_at=119 WHERE task_ordinal=2",
            [],
        )
        .unwrap();
    let question = post(
        &mut board,
        "claude",
        plan,
        EntryKind::Question,
        "Review this unresolved point",
    );
    call(
        &mut board,
        "claude",
        BoardOp::Propose {
            supersedes: None,
            base: PlanRevision::new(plan, 1).unwrap(),
            body: PlanText::new("proposal").unwrap(),
            summary: EntryText::new("proposal summary").unwrap(),
        },
    );
    call(
        &mut board,
        "codex",
        BoardOp::Feedback {
            kind: FeedbackKind::Wrong,
            summary: EntryText::new("missing evidence").unwrap(),
            body: None,
            plan: Some(plan),
            metadata: FeedbackMetadata::default(),
            import_key: None,
        },
    );
    let BoardResult::Review(evidence) = call(
        &mut board,
        "codex",
        BoardOp::Review {
            base: PlanRevision::new(plan, 1).unwrap(),
            agent: Some(HarnessLabel::parse("codex").unwrap()),
        },
    ) else {
        panic!("missing review")
    };
    assert_eq!(evidence.claims.len(), 1);
    assert_eq!(evidence.claims[0].scope.as_str(), "scope 1");
    assert_eq!(evidence.claims[0].ended_at, Some(130));
    assert_eq!(evidence.open_questions[0].id, question);
    assert_eq!(evidence.open_proposals.len(), 1);
    assert_eq!(evidence.open_feedback.len(), 1);
    assert!(
        evidence
            .entries
            .iter()
            .all(|entry| entry.actor.harness.as_str() == "codex")
    );
}

#[test]
fn repositories_keep_all_plan_links_each_path_and_the_oldest_plan_boundary() {
    let (directory, mut board) = database();
    let first = new_plan(&mut board);
    let BoardResult::Change(second) = call(
        &mut board,
        "human",
        BoardOp::New {
            title: PlanTitle::new("Other plan").unwrap(),
            body: PlanText::new("Other scope").unwrap(),
            steward: None,
        },
    ) else {
        panic!("missing second plan")
    };
    let second = second.plan.unwrap();
    board
        .conn
        .execute(
            "UPDATE plans SET created_at=CASE id WHEN 1 THEN 120 ELSE 90 END",
            [],
        )
        .unwrap();
    let repo_key: RepoKey = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap();
    let first_path = directory.path().join("one.git");
    let second_path = directory.path().join("two.git");
    std::fs::create_dir_all(&first_path).unwrap();
    std::fs::create_dir_all(&second_path).unwrap();
    for (plan, path) in [(first, &first_path), (second, &second_path)] {
        call(
            &mut board,
            "codex",
            BoardOp::RegisterRepo {
                registration: RepoRegistration {
                    root_commits: Vec::new(),
                    registration_error: None,
                    origin_override: None,
                    repo_key: repo_key.clone(),
                    origin_label: Some("origin".to_owned()),
                    host: "laptop".to_owned(),
                    common_dir: path.to_owned(),
                    plan_id: Some(plan),
                },
            },
        );
    }
    board
        .conn
        .execute(
            "UPDATE repo_paths SET scan_error='failed scan' WHERE common_dir=?1",
            [second_path.to_str().unwrap()],
        )
        .unwrap();
    let BoardResult::Repositories(targets) = call(
        &mut board,
        "codex",
        BoardOp::Repositories { plan: Some(first) },
    ) else {
        panic!("missing repositories")
    };
    assert_eq!(targets.len(), 2);
    assert!(targets.iter().all(|target| target.plans == [first, second]));
    assert!(targets.iter().all(|target| target.oldest_plan_at == 90));
    assert!(
        targets
            .iter()
            .all(|target| target.registration.plan_id == Some(first))
    );
    assert!(
        targets
            .iter()
            .any(|target| target.scan_error.as_deref() == Some("failed scan"))
    );
}

#[test]
fn review_commits_keep_coauthors_and_links_to_other_plans() {
    let (directory, mut board) = database();
    let first = new_plan(&mut board);
    let BoardResult::Change(second) = call(
        &mut board,
        "human",
        BoardOp::New {
            title: PlanTitle::new("Other plan").unwrap(),
            body: PlanText::new("Other scope").unwrap(),
            steward: None,
        },
    ) else {
        panic!("missing second plan")
    };
    let second = second.plan.unwrap();
    board.conn.execute("UPDATE entries SET created_at=120 WHERE id=(SELECT entry_id FROM revisions WHERE plan_id=1 AND number=1)", []).unwrap();
    let repo_key: RepoKey = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap();
    call(
        &mut board,
        "codex",
        BoardOp::RegisterRepo {
            registration: RepoRegistration {
                root_commits: Vec::new(),
                registration_error: None,
                origin_override: None,
                repo_key: repo_key.clone(),
                origin_label: None,
                host: "laptop".to_owned(),
                common_dir: directory.path().to_owned(),
                plan_id: Some(first),
            },
        },
    );
    let commit = LinkedCommit {
        repo_key,
        oid: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".parse().unwrap(),
        subject: "Implement both plans".to_owned(),
        committed_at: 125,
        author: "Josh".to_owned(),
        coauthors: vec![CommitCoauthor {
            harness: HarnessLabel::parse("codex").unwrap(),
            model: "gpt-6.1-sol".to_owned(),
            email: "noreply@openai.com".to_owned(),
        }],
        files: 1,
        insertions: 3,
        deletions: 2,
        plans: vec![
            CommitPlanLink {
                plan_id: first,
                task_ordinal: None,
            },
            CommitPlanLink {
                plan_id: second,
                task_ordinal: None,
            },
        ],
    };
    call(
        &mut board,
        "codex",
        BoardOp::LinkCommits {
            commits: vec![commit.clone()],
        },
    );
    let BoardResult::Review(evidence) = call(
        &mut board,
        "codex",
        BoardOp::Review {
            base: PlanRevision::new(first, 1).unwrap(),
            agent: None,
        },
    ) else {
        panic!("missing review")
    };
    assert_eq!(evidence.commits, [commit]);
}

#[test]
fn show_entry_recovers_full_large_proposal_and_keeps_decided_evidence() {
    let (_directory, mut board) = database();
    let plan = new_plan(&mut board);
    let body = "proposal body with concrete evidence\n".repeat(850);
    assert!((30_000..32_768).contains(&body.len()));
    let BoardResult::Change(proposed) = call(
        &mut board,
        "codex",
        BoardOp::Propose {
            supersedes: None,
            base: PlanRevision::new(plan, 1).unwrap(),
            body: PlanText::new(body.clone()).unwrap(),
            summary: EntryText::new("compact proposal summary").unwrap(),
        },
    ) else {
        panic!("missing proposal");
    };
    let BoardResult::Entry(view) = call(
        &mut board,
        "human",
        BoardOp::Show {
            target: Some(BoardRef::Entry(proposed.entry)),
        },
    ) else {
        panic!("missing entry view");
    };
    assert_eq!(view.proposal.as_ref().unwrap().body.as_str(), body);
    assert!(view.can_decide);
    assert_eq!(view.plan_head_revision, Some(1));
    let rendered = crate::board::board_render::render_reply(
        &BoardReply::new("local", BoardResult::Entry(view)),
        &crate::output::OutputBudget::new(32768)
            .unwrap()
            .with_format(crate::output::OutputFormat::Json),
    )
    .unwrap();
    let json: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
    assert_eq!(json["result"]["data"]["proposal"]["body"], body);
    call(
        &mut board,
        "human",
        BoardOp::Accept {
            proposal: proposed.entry,
            note: None,
        },
    );
    let BoardResult::Entry(decided) = call(
        &mut board,
        "codex",
        BoardOp::Show {
            target: Some(BoardRef::Entry(proposed.entry)),
        },
    ) else {
        panic!("missing decided proposal");
    };
    assert_eq!(decided.proposal.as_ref().unwrap().body.as_str(), body);
    assert_eq!(
        decided.proposal.as_ref().unwrap().state,
        ProposalState::Accepted
    );
    assert!(!decided.can_decide);
    assert!(decided.can_supersede);
    let question = post(
        &mut board,
        "claude",
        plan,
        EntryKind::Question,
        "question to answer",
    );
    let answer = post(
        &mut board,
        "codex",
        plan,
        EntryKind::Answer,
        &format!("answer {question}"),
    );
    let BoardResult::Entry(question) = call(
        &mut board,
        "human",
        BoardOp::Show {
            target: Some(BoardRef::Entry(question)),
        },
    ) else {
        panic!("missing question");
    };
    assert_eq!(question.replies[0].id, answer);
    assert!(question.backrefs.iter().any(|entry| entry.id == answer));
    assert!(!question.can_answer);
}
