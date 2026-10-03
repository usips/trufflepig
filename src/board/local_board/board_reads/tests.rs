mod entry_permission_tests;
mod proposal_entry_view_tests;
mod repository_evidence_tests;
mod review_window_tests;

use super::*;
use crate::board::board_actor::BoardActor;
use crate::board::board_backend::BoardBackend;
use crate::board::board_ids::{RevisionSpan, TaskId};
use crate::board::board_vocabulary::{FeedbackKind, TaskColumn};
use crate::board::local_board::LocalBoard;
use std::time::Duration;

fn database() -> (tempfile::TempDir, LocalBoard) {
    let directory = crate::board::board_test_support::scratch("board-fixture-");
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
            target: BoardRef::Plan(plan),
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
            target: BoardRef::Plan(plan),
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
            target: BoardRef::Span(RevisionSpan {
                plan,
                start: 1,
                end: None,
            }),
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
            target: BoardRef::Revision(base),
        },
    ) else {
        panic!("missing revision")
    };
    assert_eq!(first, diff.before);
}
