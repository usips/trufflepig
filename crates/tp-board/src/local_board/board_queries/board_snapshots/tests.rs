use super::*;
use crate::board::{
    board_actor::BoardActor,
    board_backend::BoardBackend,
    board_ids::{BoardRef, TaskId},
    board_protocol::{BoardReceiptState, BoardRequestKey, ClaimDelegate, ClaimResume},
    board_vocabulary::{EntryKind, EntryText, PlanText, PlanTitle},
};
use std::time::Duration;
use tp_core::request_context::HarnessLabel;

fn actor(harness: &str, session: &str) -> BoardActor {
    BoardActor::new(
        "receipt-tests",
        "local",
        HarnessLabel::parse(harness).unwrap(),
        session,
    )
    .unwrap()
}

fn database() -> (tempfile::TempDir, LocalBoard) {
    let directory = tp_core::test_fixtures::scratch("board-write-receipt-");
    let path = directory.path().join("board.sqlite3");
    let board = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    (directory, board)
}

fn create_plan(board: &mut LocalBoard, actor: BoardActor) -> crate::board::board_ids::PlanId {
    let reply = board
        .handle(&BoardRequest::new(
            actor,
            BoardOp::New {
                title: PlanTitle::new("Write receipt lookup").unwrap(),
                body: PlanText::new("# Receipt").unwrap(),
                repo_key: None,
            },
        ))
        .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("expected plan creation change");
    };
    change.plan.unwrap()
}

fn unknown_receipt(reply: BoardReply) {
    let BoardResult::Receipt(receipt) = reply.result else {
        panic!("expected receipt result");
    };
    assert_eq!(receipt.state, BoardReceiptState::Unknown);
    assert!(receipt.change.is_none());
}

#[test]
fn committed_post_receipt_survives_a_lost_reply_and_hides_other_actors() {
    let (_directory, mut board) = database();
    let writer = actor("codex", "writer");
    let plan = create_plan(&mut board, writer.clone());
    let request = BoardRequest::new(
        writer.clone(),
        BoardOp::Post {
            target: BoardRef::Plan(plan),
            kind: EntryKind::Note,
            body: EntryText::new("The write committed before its reply was read.").unwrap(),
            to: None,
            supersedes: None,
            progress: None,
            verdict: None,
        },
    );
    let key = request.dedupe_key().unwrap();

    let _lost_reply = board.handle(&request).unwrap();
    let receipt = board
        .handle(&BoardRequest::new(writer, BoardOp::Receipt { key }))
        .unwrap();
    let BoardResult::Receipt(receipt) = receipt.result else {
        panic!("expected receipt result");
    };
    assert_eq!(receipt.key, key);
    assert_eq!(receipt.state, BoardReceiptState::Committed);
    let change = receipt.change.unwrap();
    assert_eq!(change.plan, Some(plan));

    unknown_receipt(
        board
            .handle(&BoardRequest::new(
                actor("human", "other"),
                BoardOp::Receipt { key },
            ))
            .unwrap(),
    );
}

#[test]
fn absent_and_expired_receipts_report_unknown() {
    let (_directory, mut board) = database();
    let actor = actor("codex", "reader");
    let absent: BoardRequestKey = "00".repeat(32).parse().unwrap();
    unknown_receipt(
        board
            .handle(&BoardRequest::new(
                actor.clone(),
                BoardOp::Receipt { key: absent },
            ))
            .unwrap(),
    );

    let expired: BoardRequestKey = "11".repeat(32).parse().unwrap();
    let old = unix_now().unwrap() - BOARD_WRITE_RECEIPT_RETENTION_SECS - 1;
    board
        .conn
        .execute(
            "INSERT INTO operation_dedupes(dedupe_key,reply_json,created_at) VALUES(?1,'{}',?2)",
            rusqlite::params![expired.to_string(), old],
        )
        .unwrap();
    unknown_receipt(
        board
            .handle(&BoardRequest::new(actor, BoardOp::Receipt { key: expired }))
            .unwrap(),
    );
}

#[test]
fn delegated_holder_refresh_receipt_belongs_to_holder_not_event_author() {
    let (_directory, mut board) = database();
    let delegator = actor("human", "delegator");
    let holder = actor("codex", "holder");
    let plan = create_plan(&mut board, delegator.clone());
    let task = TaskId::new(plan, 1).unwrap();
    board
        .handle(&BoardRequest::new(
            delegator.clone(),
            BoardOp::TaskCreate {
                plan,
                title: PlanTitle::new("Delegated receipt").unwrap(),
                to: None,
                section: None,
            },
        ))
        .unwrap();
    board
        .handle(&BoardRequest::new(
            holder.clone(),
            BoardOp::Hello {
                model: "gpt-6-luna".into(),
                effort: Some("xhigh".into()),
                worker: None,
            },
        ))
        .unwrap();
    let delegated = board
        .handle(&BoardRequest::new(
            delegator.clone(),
            BoardOp::ClaimTask {
                task,
                scope: Some(EntryText::new("delegated scope").unwrap()),
                resume: ClaimResume::No,
                delegate: Some(ClaimDelegate {
                    harness: holder.harness.clone(),
                    session: holder.session.clone(),
                }),
            },
        ))
        .unwrap();
    let BoardResult::Change(original) = delegated.result else {
        panic!("expected delegated claim change");
    };
    let refresh = BoardRequest::new(
        holder.clone(),
        BoardOp::ClaimTask {
            task,
            scope: Some(EntryText::new("refreshed scope").unwrap()),
            resume: ClaimResume::No,
            delegate: None,
        },
    );
    let key = refresh.dedupe_key().unwrap();
    let refreshed = board.handle(&refresh).unwrap();
    let BoardResult::Change(refreshed) = refreshed.result else {
        panic!("expected delegated-holder refresh change");
    };
    assert_eq!(refreshed.entry, original.entry);
    assert_eq!(refreshed.seq, original.seq);

    let event_author: String = board
        .conn
        .query_row(
            concat!(
                "SELECT a.session FROM events e JOIN actors a ON a.id=e.actor_id ",
                "WHERE e.seq=?1"
            ),
            [sql_number(original.seq.get())],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(event_author, delegator.session);

    let holder_receipt = board
        .handle(&BoardRequest::new(holder, BoardOp::Receipt { key }))
        .unwrap();
    let BoardResult::Receipt(holder_receipt) = holder_receipt.result else {
        panic!("expected holder receipt");
    };
    assert_eq!(holder_receipt.state, BoardReceiptState::Committed);
    assert_eq!(holder_receipt.change.unwrap().seq, original.seq);

    unknown_receipt(
        board
            .handle(&BoardRequest::new(delegator, BoardOp::Receipt { key }))
            .unwrap(),
    );
}
