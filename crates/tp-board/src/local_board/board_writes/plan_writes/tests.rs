use super::*;
use crate::board_actor::BoardRecipient;
use crate::board_backend::BoardBackend;
use crate::board_ids::PlanRevision;
use crate::board_protocol::AuthorityRole;
use std::time::Duration;

mod approval_guidance;
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
    let directory = tp_core::test_fixtures::scratch("board-test-");
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
            repo_key: None,
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
fn new_plan_links_a_chosen_repository_and_rejects_unknown_keys() {
    let (_directory, mut board) = database();
    let repo = RepoKey::from_roots([tp_core::identity::GitOid::parse(
        "4444444444444444444444444444444444444444",
    )
    .unwrap()])
    .unwrap();
    board
        .conn
        .execute("INSERT INTO repos(repo_key) VALUES(?1)", [repo.as_str()])
        .unwrap();
    let created = call(
        &mut board,
        actor("josh", "human", "owner"),
        BoardOp::New {
            title: PlanTitle::new("Linked plan").unwrap(),
            body: PlanText::new("").unwrap(),
            repo_key: Some(repo.clone()),
        },
    );
    let plan = created.plan.unwrap();
    let linked: String = board
        .conn
        .query_row(
            "SELECT repo_key FROM plan_repos WHERE plan_id=?1",
            [sql_number(plan.get())],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(linked, repo.as_str());
    let plans_before: i64 = board
        .conn
        .query_row("SELECT COUNT(*) FROM plans", [], |row| row.get(0))
        .unwrap();
    let unknown = RepoKey::from_roots([tp_core::identity::GitOid::parse(
        "5555555555555555555555555555555555555555",
    )
    .unwrap()])
    .unwrap();
    let error = board
        .handle(&BoardRequest::new(
            actor("josh", "human", "owner"),
            BoardOp::New {
                title: PlanTitle::new("Unknown repository").unwrap(),
                body: PlanText::new("").unwrap(),
                repo_key: Some(unknown),
            },
        ))
        .unwrap_err();
    assert_eq!(error.code, BoardErrorCode::InvalidReference);
    let plans_after: i64 = board
        .conn
        .query_row("SELECT COUNT(*) FROM plans", [], |row| row.get(0))
        .unwrap();
    assert_eq!(plans_before, plans_after, "a rejected link creates no plan");
}

#[test]
fn direct_edits_enforce_owner_and_granted_decide_authority_atomically() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    let grant = grant_decide(&mut board, plan, "claude");
    let base = PlanRevision::new(plan, 1).unwrap();
    for denied in [
        actor("josh", "codex", "same-owner"),
        actor("other", "human", "other-owner"),
        actor("other", "claude", "different-user"),
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
        if harness == "claude" {
            let authority: (String, Option<i64>) = board
                .conn
                .query_row(
                    "SELECT authority_basis,authority_entry FROM entries WHERE id=?1",
                    [sql_number(edited.entry.get())],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(authority.0, "grant");
            assert_eq!(authority.1, Some(i64::try_from(grant.get()).unwrap()));
        }
    }
}

fn grant_decide(board: &mut LocalBoard, plan: PlanId, grantee: &str) -> EntryId {
    call(
        board,
        actor("josh", "human", "owner"),
        BoardOp::Grant {
            plan: Some(plan),
            roles: vec![AuthorityRole::Decide],
            grantee: BoardRecipient::parse(grantee).unwrap(),
            expires_in: None,
            note: None,
        },
    )
    .entry
}
