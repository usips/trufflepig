use crate::board::board_actor::{BoardActor, BoardRecipient, HarnessLabel};
use crate::board::board_backend::BoardBackend;
use crate::board::board_ids::{EntryId, PlanId};
use crate::board::board_protocol::{
    AuthorityRole, BoardError, BoardErrorCode, BoardOp, BoardRequest, BoardResult, ReadScope,
};
use crate::board::board_vocabulary::{EntryKind, PlanText, PlanTitle};
use crate::board::local_board::LocalBoard;
use std::time::Duration;

fn database() -> (tempfile::TempDir, LocalBoard) {
    let directory = crate::test_fixtures::scratch("board-authority-");
    let board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(120),
    )
    .unwrap();
    (directory, board)
}

fn actor(harness: &str, session: &str) -> BoardActor {
    BoardActor::new(
        "josh",
        "laptop",
        HarnessLabel::parse(harness).unwrap(),
        session,
    )
    .unwrap()
}

fn create_plan(board: &mut LocalBoard) -> PlanId {
    let BoardResult::Change(change) = board
        .handle(&BoardRequest::new(
            actor("human", "owner"),
            BoardOp::New {
                title: PlanTitle::new("Grant audit").unwrap(),
                body: PlanText::new("# Scope").unwrap(),
                repo_key: None,
            },
        ))
        .unwrap()
        .result
    else {
        panic!("expected plan change");
    };
    change.plan.unwrap()
}

fn grant(board: &mut LocalBoard, plan: PlanId, grantee: &str) -> Result<EntryId, BoardError> {
    let reply = board.handle(&BoardRequest::new(
        actor("human", "owner"),
        BoardOp::Grant {
            plan: Some(plan),
            roles: vec![AuthorityRole::Triage],
            grantee: BoardRecipient::parse(grantee).unwrap(),
            expires_in: None,
            note: None,
        },
    ))?;
    let BoardResult::Change(change) = reply.result else {
        panic!("expected grant change");
    };
    Ok(change.entry)
}

#[test]
fn revocation_is_addressed_to_the_grantee_inbox() {
    let (_directory, mut board) = database();
    let plan = create_plan(&mut board);
    let codex = actor("codex", "s1");
    let entry = grant(&mut board, plan, "josh@laptop/codex/s1").unwrap();
    board
        .handle(&BoardRequest::new(
            actor("human", "owner"),
            BoardOp::Revoke { entry },
        ))
        .unwrap();
    let reply = board
        .handle(&BoardRequest::new(
            codex.clone(),
            BoardOp::Inbox {
                scope: ReadScope::All,
                after: None,
                limit: 50,
            },
        ))
        .unwrap();
    let BoardResult::Inbox(inbox) = reply.result else {
        panic!("expected inbox");
    };
    assert!(
        inbox.events.iter().any(|event| {
            event.kind == EntryKind::Revoke && event.to == Some(BoardRecipient::for_actor(&codex))
        }),
        "{:?}",
        inbox.events
    );
}

#[test]
fn reserved_labels_are_refused_as_grantees() {
    let (_directory, mut board) = database();
    let plan = create_plan(&mut board);
    for grantee in [
        "human",
        "cli",
        "system",
        "git:dev@example.invalid",
        "josh@laptop/human/s1",
    ] {
        let error = grant(&mut board, plan, grantee).unwrap_err();
        assert_eq!(error.code, BoardErrorCode::InvalidOptions, "{grantee}");
    }
    grant(&mut board, plan, "codex").unwrap();
    grant(&mut board, plan, "codex/s1").unwrap();
}
