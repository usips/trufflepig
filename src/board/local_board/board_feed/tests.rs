use crate::board::board_protocol::ReadScope;
mod event_snapshot_tests;
mod event_vendor_tests;
mod feedback_gate_tests;
mod inbox_planless_tests;
mod inbox_reminder_cap_tests;
mod inbox_scope_tests;
mod inbox_watermark_tests;

use super::*;
use crate::board::board_actor::{BoardActor, HarnessLabel};
use crate::board::board_backend::BoardBackend;
use crate::board::board_ids::{RepoKey, TaskId};
use crate::board::board_protocol::{BoardOp, BoardRequest};
use crate::board::board_vocabulary::{EntryKind, PlanText, PlanTitle};
use crate::board::local_board::LocalBoard;
use std::time::Duration;

fn database() -> (tempfile::TempDir, LocalBoard) {
    let directory = crate::board::board_test_support::scratch("board-test-");
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

fn plan(board: &mut LocalBoard) -> PlanId {
    match call(
        board,
        "human",
        BoardOp::New {
            title: PlanTitle::new("Trial").unwrap(),
            body: PlanText::new("# Scope").unwrap(),
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
    to: Option<&str>,
) {
    call(
        board,
        harness,
        BoardOp::Post {
            target: BoardRef::Plan(plan),
            kind,
            body: EntryText::new(body).unwrap(),
            to: to.map(|value| BoardRecipient::parse(value).unwrap()),
            supersedes: None,
        },
    );
}

fn feed(board: &mut LocalBoard, after: Option<EventSeq>, limit: usize) -> InboxReply {
    match call(
        board,
        "codex",
        BoardOp::Inbox {
            scope: ReadScope::All,
            after,
            limit,
        },
    ) {
        BoardResult::Inbox(inbox) => inbox,
        other => panic!("unexpected {other:?}"),
    }
}

fn scoped_feed(board: &mut LocalBoard, scope: ReadScope, limit: usize) -> InboxReply {
    match call(
        board,
        "codex",
        BoardOp::Inbox {
            scope,
            after: Some(EventSeq::new(0)),
            limit,
        },
    ) {
        BoardResult::Inbox(inbox) => inbox,
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn first_feed_keeps_twenty_fresh_events_and_old_reminders_without_advancing() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    post(
        &mut board,
        "claude",
        plan,
        EntryKind::Question,
        "old unanswered question",
        None,
    );
    for index in 0..25 {
        post(
            &mut board,
            "claude",
            plan,
            EntryKind::Progress,
            &format!("progress {index}"),
            None,
        );
    }
    post(
        &mut board,
        "codex",
        plan,
        EntryKind::Note,
        "my own write",
        None,
    );
    let first = feed(&mut board, None, 100);
    assert_eq!(first.events.len(), 20);
    assert_eq!(first.events.first().unwrap().seq.get(), 8);
    assert_eq!(first.events.last().unwrap().seq.get(), 27);
    assert_eq!(first.open.len(), 1);
    assert_eq!(first.open[0].seq.get(), 2);
    assert_eq!(first.cursor.get(), 0);
    assert_eq!(feed(&mut board, None, 100).events, first.events);
    let through = first.events[4].seq;
    assert_eq!(
        call(
            &mut board,
            "codex",
            BoardOp::AcknowledgeInbox {
                rendered_through: through
            }
        ),
        BoardResult::Cursor(through)
    );
    let next = feed(&mut board, None, 100);
    assert_eq!(next.events, first.events[5..]);
    assert_eq!(next.open, first.open);
    let explicit = feed(&mut board, Some(EventSeq::new(0)), 100);
    assert!(!explicit.advancing);
    assert_eq!(explicit.cursor, through);
    assert_eq!(explicit.events.len(), 27);
    assert_eq!(
        call(
            &mut board,
            "codex",
            BoardOp::AcknowledgeInbox {
                rendered_through: EventSeq::new(1)
            }
        ),
        BoardResult::Cursor(through)
    );
}

#[test]
fn addressed_news_filters_recipients_while_labor_and_own_reminders_remain_visible() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    post(
        &mut board,
        "claude",
        plan,
        EntryKind::Question,
        "private question for muse",
        Some("muse"),
    );
    post(
        &mut board,
        "claude",
        plan,
        EntryKind::Note,
        "for codex",
        Some("codex"),
    );
    post(
        &mut board,
        "codex",
        plan,
        EntryKind::Question,
        "my unresolved question",
        None,
    );
    call(
        &mut board,
        "human",
        BoardOp::TaskCreate {
            plan,
            title: PlanTitle::new("Shared labor").unwrap(),
            to: Some(BoardRecipient::parse("muse").unwrap()),
            section: None,
        },
    );
    let inbox = feed(&mut board, Some(EventSeq::new(0)), 100);
    assert!(
        !inbox
            .events
            .iter()
            .any(|event| event.summary.as_str().contains("private question"))
    );
    assert!(
        inbox
            .events
            .iter()
            .any(|event| event.summary.as_str().contains("for codex"))
    );
    assert!(
        inbox
            .events
            .iter()
            .any(|event| event.kind == EntryKind::Task)
    );
    assert!(
        !inbox
            .events
            .iter()
            .any(|event| event.actor.harness.as_str() == "codex")
    );
    assert_eq!(inbox.open.len(), 1);
    assert_eq!(inbox.open[0].actor.harness.as_str(), "codex");
    assert!(
        feed(&mut board, None, 1)
            .open
            .iter()
            .any(|entry| entry.body.as_str() == "my unresolved question")
    );
}
