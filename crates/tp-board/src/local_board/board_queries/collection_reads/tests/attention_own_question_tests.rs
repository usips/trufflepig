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
        vec![id(3), id(4)],
        "a sibling session did not ask the question"
    );
    let other = authority_actor("josh", "laptop", "claude", "s9");
    assert_eq!(
        attention_ids(&attention_page(reader, &other, None, true, 200)),
        vec![id(3), id(4)],
        "only the exact asking session hides its own question"
    );
}

#[test]
fn collection_attention_shows_sibling_session_questions_in_needs_you() {
    let (_directory, board) = database();
    seed_entry(&board.conn, 3, 3, 1, "question", 1, None, "own question");
    let reader = board.reader.as_ref().expect("read connection");
    let sibling = authority_actor("josh", "laptop", "codex", "s2");
    assert_eq!(
        attention_ids(&attention_page(reader, &sibling, None, true, 200)),
        vec![id(3)],
        "a sibling session did not ask the question"
    );
}

#[test]
fn collection_attention_shows_questions_addressed_to_the_reader() {
    let (_directory, board) = database();
    seed_entry(
        &board.conn,
        3,
        3,
        1,
        "question",
        1,
        Some("codex"),
        "question for my harness",
    );
    seed_entry(
        &board.conn,
        4,
        4,
        1,
        "question",
        1,
        Some("josh@laptop/codex/s2"),
        "question for my sibling",
    );
    seed_entry(&board.conn, 5, 5, 1, "question", 1, None, "own question");
    let reader = board.reader.as_ref().expect("read connection");
    let sibling = authority_actor("josh", "laptop", "codex", "s2");
    assert_eq!(
        attention_ids(&attention_page(reader, &sibling, None, true, 200)),
        vec![id(3), id(4), id(5)],
        "siblings' questions stay visible, addressed or not"
    );
    assert_eq!(
        attention_ids(&attention_page(reader, &context(), None, true, 200)),
        vec![id(3)],
        "an addressed question reaches its reader but an unaddressed own one stays out"
    );
}
