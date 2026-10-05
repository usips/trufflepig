use super::*;

fn attention_through(
    conn: &Connection,
    ctx: &WriteContext,
    through: Option<EventSeq>,
) -> AttentionReply {
    match attention(conn, ctx, None, true, None, through, 200)
        .unwrap()
        .result
    {
        BoardResult::Attention(page) => page,
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn collection_attention_ignores_answers_and_corrections_past_through() {
    let (_directory, board) = database();
    seed_entry(&board.conn, 3, 3, 1, "question", 3, None, "answered later");
    seed_entry(&board.conn, 4, 5, 1, "answer", 1, None, "late answer");
    seed_entry(&board.conn, 5, 6, 1, "question", 3, None, "corrected later");
    seed_entry(&board.conn, 6, 8, 1, "note", 1, None, "late correction");
    board
        .conn
        .execute_batch(
            "INSERT INTO entry_refs VALUES(4,'E3'); UPDATE entries SET supersedes=5 WHERE id=6;",
        )
        .unwrap();
    let reader = board.reader.as_ref().expect("read connection");
    let ctx = context();
    assert_eq!(
        attention_ids(&attention_through(reader, &ctx, Some(EventSeq::new(4)))),
        vec![id(3)],
        "an answer past through does not close the question at that through"
    );
    assert_eq!(
        attention_ids(&attention_through(reader, &ctx, Some(EventSeq::new(7)))),
        vec![id(5)],
        "a correction past through does not close the question at that through"
    );
    assert_eq!(
        attention_ids(&attention_through(reader, &ctx, None)),
        Vec::new(),
        "both questions close once their resolutions are visible"
    );
}

#[test]
fn collection_attention_reads_proposal_currency_as_of_through() {
    let (_directory, board) = database();
    seed_entry(
        &board.conn,
        3,
        3,
        1,
        "proposal",
        4,
        None,
        "proposal on base one",
    );
    seed_entry(&board.conn, 4, 4, 1, "note", 1, None, "filler");
    seed_entry(&board.conn, 5, 5, 1, "note", 1, None, "head advance");
    seed_entry(&board.conn, 6, 6, 1, "note", 1, None, "later filler");
    board
        .conn
        .execute_batch(concat!(
            "INSERT INTO proposals VALUES(3,1,1,'one','open',NULL,NULL); ",
            "UPDATE plans SET head_revision=2 WHERE id=1; ",
            "INSERT INTO revisions VALUES(1,2,'one','accept',5,1,5);"
        ))
        .unwrap();
    let reader = board.reader.as_ref().expect("read connection");
    let human = authority_actor("josh", "laptop", "human", "h1");
    assert_eq!(
        attention_ids(&attention_through(reader, &human, Some(EventSeq::new(4)))),
        vec![id(3)],
        "the proposal was current before the head advanced"
    );
    assert_eq!(
        attention_ids(&attention_through(reader, &human, None)),
        Vec::new(),
        "the proposal is stale at the live head"
    );
    let author = authority_actor("other", "laptop", "codex", "s1");
    assert!(
        attention_through(reader, &author, Some(EventSeq::new(4)))
            .rebase_needed
            .is_empty(),
        "no rebase is needed as of a through before the head advanced"
    );
    assert_eq!(
        attention_through(reader, &author, None).rebase_needed,
        vec![id(3)]
    );
}
