mod entry_permission_tests;
mod proposal_entry_view_tests;
mod reminder_index_tests;
mod reminder_session_tests;
mod reminder_through_tests;
mod repository_evidence_tests;
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
    assert_eq!(view.entries_next_after, None);
    assert_eq!(view.entries_next_before, None);
    assert!(
        view.entries
            .windows(2)
            .all(|pair| (pair[0].seq, pair[0].id) > (pair[1].seq, pair[1].id)),
        "plan windows read newest-first"
    );
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
fn plan_windows_omit_oldest_entries_behind_a_before_cursor() {
    let (_directory, board) = database();
    board
        .conn
        .execute_batch(
            "INSERT INTO actors VALUES(1,'josh','laptop','codex','s1');
             INSERT INTO plans(id,title,owner_user,head_revision,created_at)
             VALUES(1,'One','josh',1,1);
             INSERT INTO texts VALUES('one','body');
             INSERT INTO entries(id,plan_id,kind,body,actor_id,seq,created_at)
             VALUES(1,1,'create','One',1,1,50);
             INSERT INTO revisions VALUES(1,1,'one','create',1,1,1);",
        )
        .unwrap();
    for number in 2..=202u64 {
        board
            .conn
            .execute(
                "INSERT INTO entries(id,plan_id,kind,body,actor_id,seq,created_at) \
                 VALUES(?1,1,'question','open?',1,?2,50)",
                params![number as i64, number as i64],
            )
            .unwrap();
    }
    let ctx = WriteContext {
        actor_id: 1,
        actor: BoardActor::new(
            "josh",
            "laptop",
            HarnessLabel::parse("codex").unwrap(),
            "session1",
        )
        .unwrap(),
        model: None,
        effort: None,
        now: 100,
        seq: EventSeq::new(0),
        claim_ttl_secs: 20,
        via: None,
    };
    let view = plan_view(&board.conn, &ctx, PlanId::new(1).unwrap()).unwrap();
    assert_eq!(view.entries.len(), 200);
    assert_eq!(view.entries_omitted, 1);
    assert_eq!(view.entries.first().unwrap().id.get(), 202);
    assert_eq!(view.entries.last().unwrap().id.get(), 3);
    assert_eq!(
        view.entries_next_before,
        Some(EntryCursor {
            seq: EventSeq::new(3),
            entry: EntryId::new(3).unwrap()
        })
    );
    assert_eq!(view.entries_next_after, None);
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

#[test]
fn stale_proposal_reminders_stay_visible_only_to_their_author() {
    let (_directory, mut board) = database();
    let plan = new_plan(&mut board);
    let base = PlanRevision::new(plan, 1).unwrap();
    let BoardResult::Change(stale) = call(
        &mut board,
        "codex",
        BoardOp::Propose {
            supersedes: None,
            base,
            body: PlanText::new("# Stale").unwrap(),
            summary: EntryText::new("stale proposal reminder").unwrap(),
        },
    ) else {
        panic!("missing stale proposal")
    };
    let BoardResult::Change(advance) = call(
        &mut board,
        "muse",
        BoardOp::Propose {
            supersedes: None,
            base,
            body: PlanText::new("# Advanced").unwrap(),
            summary: EntryText::new("advance the head").unwrap(),
        },
    ) else {
        panic!("missing head advance")
    };
    call(
        &mut board,
        "human",
        BoardOp::Accept {
            proposal: advance.entry,
            note: None,
        },
    );
    let reminders = |board: &mut LocalBoard, user: &str, harness: &str| {
        let actor = BoardActor::new(
            user,
            "laptop",
            HarnessLabel::parse(harness).unwrap(),
            "session1",
        )
        .unwrap();
        match board
            .handle(&BoardRequest::new(
                actor,
                BoardOp::Inbox {
                    after: Some(EventSeq::new(0)),
                    limit: 100,
                    repo_key: None,
                    all: true,
                },
            ))
            .unwrap()
            .result
        {
            BoardResult::Inbox(inbox) => inbox.open,
            other => panic!("unexpected {other:?}"),
        }
    };
    let author = reminders(&mut board, "josh", "codex");
    assert!(
        author.iter().any(|entry| entry.id == stale.entry),
        "author reminders keep the stale proposal: {author:?}"
    );
    for (user, harness) in [("josh", "muse"), ("other", "codex")] {
        let open = reminders(&mut board, user, harness);
        assert!(
            !open.iter().any(|entry| entry.id == stale.entry),
            "{user}/{harness} reminders omit another author's stale proposal"
        );
    }
    let BoardResult::Change(current) = call(
        &mut board,
        "codex",
        BoardOp::Propose {
            supersedes: None,
            base: PlanRevision::new(plan, 2).unwrap(),
            body: PlanText::new("# Current").unwrap(),
            summary: EntryText::new("current proposal reminder").unwrap(),
        },
    ) else {
        panic!("missing current proposal")
    };
    let other = reminders(&mut board, "josh", "muse");
    assert!(
        other.iter().any(|entry| entry.id == current.entry),
        "current proposals keep their existing visibility"
    );
}
