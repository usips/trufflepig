use super::*;
use crate::board::board_protocol::ReadScope;

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
            "UPDATE plans SET head_revision=2; ",
            "INSERT INTO revisions VALUES(1,2,'one','accept',9,1,9),(2,2,'one','accept',10,1,10);"
        ))
        .unwrap();
    let mut ctx = context();
    ctx.actor_id = -1;
    let BoardResult::Attention(result) = attention(
        board.reader.as_ref().expect("read connection"),
        &ctx,
        &(ReadScope::Repo(repo.clone())),
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
        &(ReadScope::Repo(repo.clone())),
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
        &ReadScope::All,
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
        &ReadScope::All,
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
