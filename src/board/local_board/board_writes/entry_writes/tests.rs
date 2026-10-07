use super::*;
use crate::board::board_ids::TaskId;
use crate::board::board_vocabulary::{PlanText, PlanTitle};

fn manual_link_actor(user: &str, harness: &str) -> BoardActor {
    BoardActor::new(
        user,
        "fixture-host",
        HarnessLabel::parse(harness).unwrap(),
        "linker",
    )
    .unwrap()
}

fn manual_link_plan(board: &mut LocalBoard) -> (BoardActor, TaskId) {
    let owner = manual_link_actor("fixture", "human");
    let reply = board
        .handle(&BoardRequest::new(
            owner.clone(),
            BoardOp::New {
                title: PlanTitle::new("Manual links").unwrap(),
                body: PlanText::new("Manual links").unwrap(),
                steward: Some(HarnessLabel::parse("codex").unwrap()),
                repo_key: None,
            },
        ))
        .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("plan result")
    };
    let plan = change.plan.unwrap();
    board
        .handle(&BoardRequest::new(
            owner.clone(),
            BoardOp::TaskCreate {
                plan,
                title: PlanTitle::new("repair").unwrap(),
                to: None,
                section: None,
            },
        ))
        .unwrap();
    (owner, TaskId::new(plan, 1).unwrap())
}

fn manual_link_resolution() -> LinkedCommit {
    let oid = crate::identity::GitOid::parse(&"b".repeat(40)).unwrap();
    LinkedCommit {
        repo_key: RepoKey::from_roots([oid]).unwrap(),
        oid,
        subject: "repaired by hand".into(),
        committed_at: 12,
        author: "Fixture <fixture@example.test>".into(),
        coauthors: Vec::new(),
        files: 1,
        insertions: 2,
        deletions: 3,
        plans: Vec::new(),
    }
}

fn manual_link_count(board: &LocalBoard, sql: &str) -> i64 {
    board
        .conn
        .query_row(sql, [], |row| row.get::<_, i64>(0))
        .unwrap()
}

mod commit_task_tests;
mod commit_unlink_edge_tests;
mod commit_unlink_tests;
mod manual_link_receipt_tests;
mod manual_link_tests;
mod repo_identity_tests;
mod repo_rekey_tests;
