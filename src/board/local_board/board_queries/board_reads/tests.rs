mod entry_permission_tests;
mod plan_show_tests;
mod plan_window_tests;
mod proposal_entry_view_tests;
mod reminder_author_tests;
mod reminder_index_tests;
mod reminder_session_tests;
mod reminder_through_tests;
mod repository_evidence_tests;
mod review_manual_link_tests;
mod review_window_tests;
mod shared_section_tests;

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
            repo_key: None,
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
