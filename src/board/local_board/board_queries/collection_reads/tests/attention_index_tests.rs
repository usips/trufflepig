use super::*;
use crate::board::board_protocol::ReadScope;

#[test]
fn attention_entry_query_drives_from_the_kind_index() {
    let (_directory, board) = database();
    let predicate =
        crate::board::local_board::board_queries::collection_reads::action_collection_reads::attention_predicate();
    let plan: Vec<String> = board
        .conn
        .prepare(&format!(
            concat!(
                "EXPLAIN QUERY PLAN SELECT e.id FROM entries e JOIN actors a ON a.id=e.actor_id ",
                "WHERE {predicate} ORDER BY e.seq,e.id LIMIT ?11"
            ),
            predicate = predicate
        ))
        .unwrap()
        .query_map(
            params![
                "josh",
                "laptop",
                "codex",
                "s1",
                "identity",
                false,
                None::<&str>,
                0i64,
                0i64,
                i64::MAX,
                11i64
            ],
            |row| row.get::<_, String>(3),
        )
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|detail| detail.starts_with("SEARCH e USING INDEX entries_kind_state")),
        "attention must drive the entries scan from entries_kind_state: {plan:?}"
    );
    assert!(
        !plan.iter().any(|detail| detail.starts_with("SCAN e")),
        "attention must not scan the whole entries table: {plan:?}"
    );
}

#[test]
fn attention_mixes_kinds_across_states_with_exact_omitted_counts() {
    let (_directory, board) = database();
    seed_entry(&board.conn, 3, 3, 1, "question", 3, None, "open question");
    seed_entry(
        &board.conn,
        4,
        4,
        1,
        "question",
        3,
        None,
        "answered question",
    );
    seed_entry(
        &board.conn,
        5,
        5,
        1,
        "feedback",
        4,
        None,
        "foreign open feedback",
    );
    seed_entry(
        &board.conn,
        6,
        6,
        1,
        "feedback",
        1,
        None,
        "own triaged feedback",
    );
    seed_entry(
        &board.conn,
        7,
        7,
        1,
        "feedback",
        1,
        None,
        "own closed feedback",
    );
    seed_entry(
        &board.conn,
        8,
        8,
        1,
        "proposal",
        3,
        None,
        "current proposal",
    );
    seed_entry(&board.conn, 9, 9, 1, "note", 1, None, "plain note");
    seed_entry(&board.conn, 10, 10, 1, "decision", 1, None, "decision");
    seed_entry(&board.conn, 11, 11, 1, "answer", 3, None, "answer");
    seed_entry(
        &board.conn,
        12,
        12,
        1,
        "question",
        4,
        None,
        "foreign question",
    );
    board
        .conn
        .execute_batch(
            "UPDATE entries SET state='open' WHERE id IN (5,8);
             UPDATE entries SET state='triaged' WHERE id=6;
             UPDATE entries SET state='closed' WHERE id=7;
             INSERT INTO entry_refs VALUES(11,'E4');
             INSERT INTO proposals VALUES(8,1,1,'one','open',NULL,NULL);",
        )
        .unwrap();
    let reader = board.reader.as_ref().expect("read connection");
    let human = authority_actor("josh", "laptop", "human", "h1");
    let first = attention_page(reader, &human, None, true, 2);
    assert_eq!(attention_ids(&first), vec![id(3), id(5)]);
    assert_eq!(first.entries_omitted, 3);
    let BoardResult::Attention(second) = attention(
        reader,
        &human,
        &ReadScope::All,
        first.next_after,
        Some(first.through),
        2,
    )
    .unwrap()
    .result
    else {
        panic!("attention");
    };
    assert_eq!(attention_ids(&second), vec![id(6), id(8)]);
    assert_eq!(second.entries_omitted, 1);
    let BoardResult::Attention(third) = attention(
        reader,
        &human,
        &ReadScope::All,
        second.next_after,
        Some(second.through),
        2,
    )
    .unwrap()
    .result
    else {
        panic!("attention");
    };
    assert_eq!(attention_ids(&third), vec![id(12)]);
    assert_eq!(third.entries_omitted, 0);
    assert_eq!(third.next_after, None);
    let full = attention_page(reader, &human, None, true, 200);
    assert_eq!(
        attention_ids(&full),
        vec![id(3), id(5), id(6), id(8), id(12)]
    );
    assert_eq!(full.entries_omitted, 0);
    let ordinary = attention_page(reader, &context(), None, true, 200);
    assert_eq!(attention_ids(&ordinary), vec![id(3), id(6), id(12)]);
    assert_eq!(ordinary.entries_omitted, 0);
}
