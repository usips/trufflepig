mod collection_snapshot_tests;
mod commit_transaction_tests;
mod connection_lifecycle_tests;
mod query_only_dispatch_tests;

use super::*;
use crate::board::board_vocabulary::{EntryText, PlanText, PlanTitle};

fn actor(harness: &str, session: &str) -> BoardActor {
    BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse(harness).unwrap(),
        session,
    )
    .unwrap()
}

fn database() -> (LocalBoard, PathBuf) {
    let path = crate::board::board_test_support::scratch("board-storage-")
        .keep()
        .join("board.sqlite3");
    (
        LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap(),
        path,
    )
}

fn new_plan(board: &mut LocalBoard, author: BoardActor, title: &str) -> BoardChange {
    let reply = board
        .handle(&BoardRequest::new(
            author,
            BoardOp::New {
                title: PlanTitle::new(title).unwrap(),
                body: PlanText::new("# Scope\noriginal").unwrap(),
                steward: Some(HarnessLabel::parse("claude").unwrap()),
                repo_key: None,
            },
        ))
        .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("new plan result");
    };
    change
}

#[test]
fn local_board_immutable_revisions_cas_and_owner_authority() {
    let (mut board, path) = database();
    let created = new_plan(&mut board, actor("human", "h1"), "Trial");
    let plan = created.plan.unwrap();
    assert_eq!(created.revision.unwrap().to_string(), "P1@1");
    let base = crate::board::board_ids::PlanRevision::new(plan, 1).unwrap();
    let proposed = board
        .handle(&BoardRequest::new(
            actor("codex", "c1"),
            BoardOp::Propose {
                supersedes: None,
                base,
                body: PlanText::new("accepted body").unwrap(),
                summary: EntryText::new("proposed").unwrap(),
            },
        ))
        .unwrap();
    let BoardResult::Change(proposed) = proposed.result else {
        panic!("proposal result");
    };
    let unauthorized = board
        .handle(&BoardRequest::new(
            actor("muse", "m1"),
            BoardOp::Accept {
                proposal: proposed.entry,
                note: None,
            },
        ))
        .unwrap_err();
    assert_eq!(unauthorized.code, BoardErrorCode::InvalidActor);
    let accepted = board
        .handle(&BoardRequest::new(
            actor("claude", "a1"),
            BoardOp::Accept {
                proposal: proposed.entry,
                note: None,
            },
        ))
        .unwrap();
    let BoardResult::Change(accepted) = accepted.result else {
        panic!("accept result");
    };
    assert_eq!(accepted.revision.unwrap().revision, 2);
    let stale = board
        .handle(&BoardRequest::new(
            actor("human", "h1"),
            BoardOp::Edit {
                base,
                body: PlanText::new("stale").unwrap(),
                summary: EntryText::new("stale edit").unwrap(),
            },
        ))
        .unwrap_err();
    assert_eq!(stale.code, BoardErrorCode::StaleRevision);
    let result = board
        .handle(&BoardRequest::new(
            actor("human", "h1"),
            BoardOp::Show {
                target: BoardRef::Revision(base),
            },
        ))
        .unwrap();
    let BoardResult::Revision(revision) = result.result else {
        panic!("revision result");
    };
    assert_eq!(revision.body.as_str(), "# Scope\noriginal");
    drop(board);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn local_board_dedupe_distinguishes_target_and_snapshots_claims() {
    let (mut board, path) = database();
    let author = actor("codex", "c1");
    let p1 = new_plan(&mut board, actor("human", "h1"), "One")
        .plan
        .unwrap();
    let p2 = new_plan(&mut board, actor("human", "h1"), "Two")
        .plan
        .unwrap();
    let op = |plan| BoardOp::Post {
        target: BoardRef::Plan(plan),
        kind: EntryKind::Progress,
        body: EntryText::new("same body").unwrap(),
        to: None,
        supersedes: None,
    };
    let mut first_request = BoardRequest::new(author.clone(), op(p1));
    first_request.claims = Some(AgentClaims {
        model: Some("test-model".to_owned()),
        effort: Some("xhigh".to_owned()),
    });
    let first = board.handle(&first_request).unwrap();
    let repeat = board.handle(&first_request).unwrap();
    let second = board.handle(&BoardRequest::new(author, op(p2))).unwrap();
    let BoardResult::Change(first) = first.result else {
        panic!("first");
    };
    let BoardResult::Change(repeat) = repeat.result else {
        panic!("repeat");
    };
    let BoardResult::Change(second) = second.result else {
        panic!("second");
    };
    assert!(repeat.deduplicated);
    assert_eq!(repeat.entry, first.entry);
    assert_ne!(second.entry, first.entry);
    assert_eq!(
        read_entry(&board.conn, first.entry)
            .unwrap()
            .model
            .as_deref(),
        Some("test-model")
    );
    drop(board);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
