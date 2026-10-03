use super::*;
use crate::board::board_backend::BoardBackend;
use crate::board::board_ids::PlanRevision;
use std::time::Duration;

mod decision_events;
mod proposal_lifecycle;

fn actor(user: &str, harness: &str, session: &str) -> BoardActor {
    BoardActor::new(
        user,
        "laptop",
        HarnessLabel::parse(harness).unwrap(),
        session,
    )
    .unwrap()
}

fn database() -> (tempfile::TempDir, LocalBoard) {
    let directory = tempfile::tempdir().unwrap();
    let board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(7200),
    )
    .unwrap();
    (directory, board)
}

fn call(board: &mut LocalBoard, actor: BoardActor, op: BoardOp) -> BoardChange {
    match board.handle(&BoardRequest::new(actor, op)).unwrap().result {
        BoardResult::Change(change) => change,
        other => panic!("unexpected {other:?}"),
    }
}

fn plan(board: &mut LocalBoard) -> PlanId {
    call(
        board,
        actor("josh", "human", "owner"),
        BoardOp::New {
            title: PlanTitle::new("Trial").unwrap(),
            body: PlanText::new("# Scope\noriginal").unwrap(),
            steward: Some(HarnessLabel::parse("claude").unwrap()),
        },
    )
    .plan
    .unwrap()
}

fn snapshot(board: &LocalBoard, plan: PlanId) -> (u64, Vec<i64>) {
    let head = board
        .conn
        .query_row(
            "SELECT head_revision FROM plans WHERE id=?1",
            [sql_number(plan.get())],
            |row| row_number(row, 0),
        )
        .unwrap();
    let counts = [
        "actors",
        "agent_sessions",
        "texts",
        "entries",
        "events",
        "revisions",
        "operation_dedupes",
        "proposals",
    ]
    .map(|table| {
        board
            .conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    })
    .to_vec();
    (head, counts)
}

#[test]
fn direct_edits_enforce_owner_and_steward_authority_atomically() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    let base = PlanRevision::new(plan, 1).unwrap();
    for denied in [
        actor("josh", "codex", "same-owner"),
        actor("other", "human", "other-owner"),
        actor("other", "claude", "other-steward"),
    ] {
        let before = snapshot(&board, plan);
        let error = board
            .handle(&BoardRequest::new(
                denied,
                BoardOp::Edit {
                    base,
                    body: PlanText::new("unauthorized replacement").unwrap(),
                    summary: EntryText::new("unauthorized direct edit").unwrap(),
                },
            ))
            .unwrap_err();
        assert_eq!(error.code, BoardErrorCode::InvalidActor);
        assert_eq!(snapshot(&board, plan), before);
    }
    for (harness, revision) in [("human", 1), ("claude", 2)] {
        let edited = call(
            &mut board,
            actor("josh", harness, "authorized"),
            BoardOp::Edit {
                base: PlanRevision::new(plan, revision).unwrap(),
                body: PlanText::new(format!("revision {}", revision + 1)).unwrap(),
                summary: EntryText::new(format!("authorized edit {revision}")).unwrap(),
            },
        );
        assert_eq!(edited.revision.unwrap().revision, revision + 1);
    }
}
