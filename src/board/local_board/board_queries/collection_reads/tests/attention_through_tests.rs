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
