use super::*;

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
        Some(&repo),
        None,
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
        None,
        None,
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
    let listed =
        |repo_key: Option<&RepoKey>| match overview(reader, &context(), repo_key, None, None, 200)
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
    assert_eq!(listed(Some(&repo)), vec![plan(1), plan(3)]);
    assert_eq!(listed(None), vec![plan(1), plan(2), plan(3)]);
}
