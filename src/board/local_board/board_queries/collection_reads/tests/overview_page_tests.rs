use super::*;
use crate::board::board_domain::board_collections::OverviewCursor;
use crate::board::board_ids::EventSeq;
use crate::board::board_protocol::ReadScope;

#[test]
fn collection_overview_caps_nested_rows_and_reports_exact_omissions() {
    let (_directory, board) = database();
    let repo = register_repositories(&board.conn);
    for ordinal in 1..=25 {
        seed_task(&board.conn, ordinal);
    }
    for ordinal in 1..=21 {
        seed_claim(&board.conn, ordinal, 1, 79);
    }
    seed_entry(&board.conn, 3, 3, 1, "question", 3, None, "unanswered");
    seed_entry(&board.conn, 4, 4, 1, "question", 3, None, "answered");
    seed_entry(&board.conn, 5, 5, 1, "answer", 1, None, "answer");
    seed_entry(&board.conn, 6, 6, 1, "feedback", 1, None, "open feedback");
    seed_entry(&board.conn, 7, 7, 1, "proposal", 1, None, "proposal");
    board
        .conn
        .execute_batch(concat!(
            "INSERT INTO entry_refs VALUES(5,'E4'); UPDATE entries SET state='open' WHERE id=6; ",
            "INSERT INTO proposals VALUES(7,1,1,'one','open',NULL,NULL);"
        ))
        .unwrap();
    let BoardResult::Overview(result) = overview(
        board.reader.as_ref().expect("read connection"),
        &context(),
        &(ReadScope::Repo(repo.clone())),
        None,
        false,
        None,
        1,
    )
    .unwrap()
    .result
    else {
        panic!("overview");
    };
    assert_eq!(result.plans.len(), 1);
    assert_eq!(result.omitted, 0);
    let first = &result.plans[0];
    assert_eq!(first.tasks.len(), 20);
    assert_eq!(first.tasks_omitted, 5);
    assert_eq!(first.claims.len(), 20);
    assert_eq!(first.claims_omitted, 1);
    assert!(first.claims.iter().all(|claim| claim.stale));
    assert_eq!(
        (
            first.open_questions,
            first.open_proposals,
            first.open_feedback
        ),
        (1, 1, 1)
    );
    assert_eq!((result.server_now, result.claim_ttl_secs), (100, 20));
    let BoardResult::Overview(result) = overview(
        board.reader.as_ref().expect("read connection"),
        &context(),
        &ReadScope::All,
        None,
        false,
        None,
        1,
    )
    .unwrap()
    .result
    else {
        panic!("overview");
    };
    assert_eq!(result.omitted, 1);
}

#[test]
fn collection_overview_scoped_scope_keeps_unlinked_plans() {
    let (_directory, board) = database();
    let repo = register_repositories(&board.conn);
    board
        .conn
        .execute(
            "INSERT INTO plans(id,title,owner_user,steward,head_revision,created_at) \
             VALUES(3,'Three','josh',NULL,1,1)",
            [],
        )
        .unwrap();
    seed_entry(&board.conn, 3, 3, 3, "create", 3, None, "Three");
    board
        .conn
        .execute("INSERT INTO revisions VALUES(3,1,'one','create',3,3,3)", [])
        .unwrap();
    let reader = board.reader.as_ref().expect("read connection");
    let listed = |repo_key: Option<&RepoKey>| match overview(
        reader,
        &context(),
        &(repo_key.map_or(ReadScope::All, |key| ReadScope::Repo(key.clone()))),
        None,
        false,
        None,
        200,
    )
    .unwrap()
    .result
    {
        BoardResult::Overview(page) => page
            .plans
            .iter()
            .map(|plan| plan.plan.id)
            .collect::<Vec<_>>(),
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(listed(Some(&repo)), vec![plan(3), plan(1)]);
    assert_eq!(listed(None), vec![plan(3), plan(2), plan(1)]);
}

#[test]
fn collection_overview_orders_and_pages_by_latest_event() {
    let (_directory, board) = database();
    seed_task(&board.conn, 1);
    board
        .conn
        .execute_batch(
            "INSERT INTO plans(id,title,owner_user,head_revision,created_at) \
             VALUES(3,'Three','josh',1,5),(4,'Four','josh',1,6);",
        )
        .unwrap();
    seed_entry(&board.conn, 3, 3, 2, "task", 1, None, "Done task");
    seed_entry(&board.conn, 4, 4, 2, "progress", 1, None, "Plan two update");
    seed_entry(&board.conn, 5, 5, 3, "create", 3, None, "Three");
    seed_entry(&board.conn, 6, 6, 4, "create", 3, None, "Four");
    board
        .conn
        .execute_batch(
            "INSERT INTO revisions VALUES(3,1,'one','create',5,3,5),(4,1,'one','create',6,3,6); \
             INSERT INTO tasks VALUES(2,1,'Done task','done',NULL,NULL,3), \
                                   (4,1,'Active task','todo',NULL,NULL,7); \
             INSERT INTO events(seq,plan_id,kind,subject,actor_id,summary,created_at) \
             VALUES(7,4,'task','P4.1',3,'created P4.1',50);",
        )
        .unwrap();
    seed_entry(
        &board.conn,
        7,
        10,
        1,
        "progress",
        1,
        None,
        "Plan one update",
    );
    let reader = board.reader.as_ref().expect("read connection");

    let BoardResult::Overview(first) =
        overview(reader, &context(), &ReadScope::All, None, false, None, 1)
            .unwrap()
            .result
    else {
        panic!("overview");
    };
    assert_eq!(first.plans[0].plan.id, plan(1));
    assert_eq!(
        first.next_after,
        Some(OverviewCursor {
            seq: EventSeq::new(10),
            plan: plan(1),
        })
    );
    assert_eq!(first.omitted, 3);

    seed_entry(
        &board.conn,
        8,
        12,
        2,
        "progress",
        1,
        None,
        "Plan two changed after the frozen page",
    );

    let BoardResult::Overview(second) = overview(
        reader,
        &context(),
        &ReadScope::All,
        first.next_after,
        false,
        Some(first.through),
        1,
    )
    .unwrap()
    .result
    else {
        panic!("overview continuation");
    };
    assert_eq!(second.plans[0].plan.id, plan(4));
    assert_eq!(second.omitted, 2);

    let BoardResult::Overview(third) = overview(
        reader,
        &context(),
        &ReadScope::All,
        second.next_after,
        false,
        Some(first.through),
        1,
    )
    .unwrap()
    .result
    else {
        panic!("second overview continuation");
    };
    assert_eq!(third.plans[0].plan.id, plan(3));
    assert_eq!(third.omitted, 1);

    let BoardResult::Overview(fourth) = overview(
        reader,
        &context(),
        &ReadScope::All,
        third.next_after,
        false,
        Some(first.through),
        1,
    )
    .unwrap()
    .result
    else {
        panic!("third overview continuation");
    };
    assert_eq!(fourth.plans[0].plan.id, plan(2));
    assert_eq!(fourth.next_after, None);
    assert_eq!(fourth.omitted, 0);

    let BoardResult::Overview(active) = overview(
        reader,
        &context(),
        &ReadScope::All,
        None,
        true,
        Some(first.through),
        1,
    )
    .unwrap()
    .result
    else {
        panic!("active overview");
    };
    assert_eq!(active.plans.len(), 1);
    assert_eq!(active.plans[0].plan.id, plan(1));
    assert_eq!(active.omitted, 1);

    let BoardResult::Overview(active_second) = overview(
        reader,
        &context(),
        &ReadScope::All,
        active.next_after,
        true,
        Some(active.through),
        1,
    )
    .unwrap()
    .result
    else {
        panic!("active overview continuation");
    };
    assert_eq!(active_second.plans.len(), 1);
    assert_eq!(active_second.plans[0].plan.id, plan(4));
    assert_eq!(active_second.next_after, None);
    assert_eq!(active_second.omitted, 0);
}
