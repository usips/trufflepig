use super::*;

#[test]
fn collection_attention_excludes_own_questions_from_needs_you() {
    let (_directory, board) = database();
    seed_entry(&board.conn, 3, 3, 1, "question", 1, None, "own question");
    seed_entry(
        &board.conn,
        4,
        4,
        1,
        "question",
        3,
        None,
        "foreign question",
    );
    let reader = board.reader.as_ref().expect("read connection");
    let own = attention_page(reader, &context(), None, true, 200);
    assert_eq!(attention_ids(&own), vec![id(4)]);
    let second = authority_actor("josh", "laptop", "codex", "s2");
    assert_eq!(
        attention_ids(&attention_page(reader, &second, None, true, 200)),
        vec![id(4)],
        "a new session of the asking harness still asked the question"
    );
    let other = authority_actor("josh", "laptop", "claude", "s9");
    assert_eq!(
        attention_ids(&attention_page(reader, &other, None, true, 200)),
        vec![id(3)],
        "another harness sees the question but not its own"
    );
}
