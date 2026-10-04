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

#[test]
fn collection_attention_uses_actual_actor_and_keeps_own_feedback() {
    let (_directory, board) = database();
    let repo = register_repositories(&board.conn);
    for ordinal in 1..=4 {
        seed_task(&board.conn, ordinal);
    }
    seed_claim(&board.conn, 1, 1, 79);
    seed_claim(&board.conn, 2, 1, 80);
    seed_claim(&board.conn, 3, 2, 79);
    seed_claim(&board.conn, 4, 4, 79);
    seed_entry(&board.conn, 3, 3, 1, "question", 3, None, "local question");
    seed_entry(
        &board.conn,
        4,
        4,
        1,
        "question",
        3,
        Some("muse"),
        "other recipient",
    );
    seed_entry(
        &board.conn,
        5,
        5,
        2,
        "question",
        3,
        Some("codex"),
        "addressed remote question",
    );
    seed_entry(
        &board.conn,
        6,
        6,
        2,
        "question",
        3,
        None,
        "out of scope question",
    );
    seed_entry(
        &board.conn,
        7,
        7,
        2,
        "feedback",
        1,
        Some("muse"),
        "my remote feedback",
    );
    seed_entry(
        &board.conn,
        8,
        8,
        1,
        "feedback",
        2,
        None,
        "other host feedback",
    );
    seed_entry(
        &board.conn,
        9,
        9,
        1,
        "proposal",
        1,
        None,
        "my stale proposal",
    );
    seed_entry(
        &board.conn,
        10,
        10,
        2,
        "proposal",
        1,
        Some("muse"),
        "my remote addressed proposal",
    );
    board
        .conn
        .execute_batch(concat!(
            "UPDATE entries SET state='open' WHERE id IN (7,8); ",
            "INSERT INTO proposals VALUES(9,1,1,'one','open',NULL,NULL),(10,2,1,'one','open',NULL,NULL); ",
            "UPDATE plans SET head_revision=2;"
        ))
        .unwrap();
    let mut ctx = context();
    ctx.actor_id = -1;
    let BoardResult::Attention(result) = attention(
        board.reader.as_ref().expect("read connection"),
        &ctx,
        Some(&repo),
        false,
        None,
        None,
        200,
    )
    .unwrap()
    .result
    else {
        panic!("attention");
    };
    assert_eq!(result.actor, ctx.actor);
    assert_eq!(
        result
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![id(3), id(5), id(7), id(9), id(10)]
    );
    assert_eq!(result.stale_claims.len(), 1);
    assert_eq!(result.stale_claims[0].claim.task.ordinal, 1);
    assert_eq!(result.rebase_needed, vec![id(9), id(10)]);
    assert_eq!(
        result.entries[3].state,
        Some(EntryState::Proposal(ProposalState::Open))
    );
    let BoardResult::Attention(result) = attention(
        board.reader.as_ref().expect("read connection"),
        &ctx,
        Some(&repo),
        false,
        None,
        None,
        1,
    )
    .unwrap()
    .result
    else {
        panic!("attention");
    };
    assert_eq!(result.entries.len(), 1);
    assert_eq!(result.entries_omitted, 4);
    assert_eq!(result.claims_omitted, 0);
    let BoardResult::Attention(result) = attention(
        board.reader.as_ref().expect("read connection"),
        &ctx,
        None,
        true,
        None,
        None,
        200,
    )
    .unwrap()
    .result
    else {
        panic!("attention");
    };
    assert!(result.entries.iter().any(|entry| entry.id == id(6)));
    assert!(!result.entries.iter().any(|entry| entry.id == id(8)));
}

#[test]
fn collection_attention_preserves_trusted_feedback_via_and_attribution() {
    let (_directory, board) = database();
    board
        .conn
        .execute("UPDATE actors SET harness='human' WHERE id=1", [])
        .unwrap();
    seed_feedback(&board.conn, 3, 3, "open");
    board
        .conn
        .execute("UPDATE entries SET via='outbox' WHERE id=3", [])
        .unwrap();
    let mut ctx = context();
    ctx.actor.harness = HarnessLabel::parse("human").unwrap();
    let changes = board.conn.total_changes();
    let reply = attention(
        board.reader.as_ref().expect("read connection"),
        &ctx,
        None,
        true,
        None,
        None,
        200,
    )
    .unwrap();
    let value = serde_json::to_value(&reply).unwrap();
    assert_eq!(value["result"]["data"]["entries"][0]["via"], "outbox");
    assert_eq!(
        value["result"]["data"]["entries"][0]["actor"]["harness"],
        "human"
    );
    let rendered = crate::board::board_render::render_reply(
        &reply,
        &crate::output::OutputBudget::new(3000)
            .unwrap()
            .with_format(crate::output::OutputFormat::Lines),
    )
    .unwrap();
    assert!(rendered.text.contains("via=outbox spooled unverified"));
    assert_eq!(board.conn.total_changes(), changes);
    assert_eq!(
        board
            .reader
            .as_ref()
            .expect("read connection")
            .total_changes(),
        0
    );
}
