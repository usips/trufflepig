use super::*;
use crate::board::board_protocol::ReadScope;

#[test]
fn collection_attention_filters_current_proposal_authority_before_limit_and_count() {
    let (_directory, board) = database();
    seed_entry(
        &board.conn,
        3,
        3,
        1,
        "proposal",
        1,
        None,
        "own current proposal",
    );
    seed_entry(
        &board.conn,
        4,
        4,
        1,
        "proposal",
        3,
        None,
        "foreign current proposal",
    );
    seed_entry(
        &board.conn,
        5,
        5,
        1,
        "question",
        3,
        None,
        "applicable question",
    );
    seed_entry(
        &board.conn,
        6,
        6,
        1,
        "proposal",
        3,
        None,
        "already accepted",
    );
    seed_entry(
        &board.conn,
        7,
        7,
        2,
        "proposal",
        1,
        None,
        "another owner's plan",
    );
    board.conn.execute_batch(
        concat!(
            "INSERT INTO proposals VALUES(3,1,1,'one','open',NULL,NULL),(4,1,1,'one','open',NULL,NULL),",
            "(6,1,1,'one','accepted',NULL,NULL),(7,2,1,'one','open',NULL,NULL); ",
            "UPDATE plans SET owner_user='other' WHERE id=2;"
        )
    ).unwrap();
    let ordinary = attention_page(
        board.reader.as_ref().expect("read connection"),
        &context(),
        None,
        true,
        1,
    );
    assert_eq!(attention_ids(&ordinary), vec![id(5)]);
    assert_eq!(ordinary.entries_omitted, 0);
    assert_eq!(ordinary.next_after, None);
    assert!(ordinary.rebase_needed.is_empty());
    let human = authority_actor("josh", "laptop", "human", "h1");
    let decision = attention_page(
        board.reader.as_ref().expect("read connection"),
        &human,
        None,
        true,
        1,
    );
    assert_eq!(attention_ids(&decision), vec![id(3)]);
    assert_eq!(decision.entries_omitted, 2);
    let BoardResult::Attention(rest) = attention(
        board.reader.as_ref().expect("read connection"),
        &human,
        &ReadScope::All,
        decision.next_after,
        Some(decision.through),
        200,
    )
    .unwrap()
    .result
    else {
        panic!("attention");
    };
    assert_eq!(attention_ids(&rest), vec![id(4), id(5)]);
    let steward = authority_actor("josh", "laptop", "claude", "s2");
    assert_eq!(
        attention_ids(&attention_page(
            board.reader.as_ref().expect("read connection"),
            &steward,
            None,
            true,
            200
        )),
        vec![id(3), id(4)],
        "the steward's own user-and-harness question leaves Needs you"
    );
    let foreign_human = authority_actor("other", "laptop", "human", "h1");
    assert_eq!(
        attention_ids(&attention_page(
            board.reader.as_ref().expect("read connection"),
            &foreign_human,
            None,
            true,
            200
        )),
        vec![id(5), id(7)]
    );
    let foreign_steward = authority_actor("other", "laptop", "claude", "s2");
    assert_eq!(
        attention_ids(&attention_page(
            board.reader.as_ref().expect("read connection"),
            &foreign_steward,
            None,
            true,
            200
        )),
        vec![id(5)]
    );
}

#[test]
fn collection_attention_stale_proposals_require_exact_author_for_rebase() {
    let (_directory, board) = database();
    seed_entry(
        &board.conn,
        3,
        3,
        1,
        "proposal",
        1,
        Some("muse"),
        "own stale proposal",
    );
    seed_entry(
        &board.conn,
        4,
        4,
        1,
        "proposal",
        3,
        None,
        "foreign stale proposal",
    );
    seed_entry(&board.conn, 5, 5, 1, "question", 3, None, "question");
    board
        .conn
        .execute_batch(concat!(
            "INSERT INTO proposals VALUES(3,1,1,'one','open',NULL,NULL),(4,1,1,'one','open',NULL,NULL); ",
            "UPDATE plans SET head_revision=2 WHERE id=1; ",
            "INSERT INTO revisions VALUES(1,2,'one','accept',5,1,5);"
        ))
        .unwrap();
    let own = attention_page(
        board.reader.as_ref().expect("read connection"),
        &context(),
        None,
        false,
        1,
    );
    assert_eq!(attention_ids(&own), vec![id(3)]);
    assert_eq!(own.rebase_needed, vec![id(3)]);
    assert_eq!(
        own.entries_omitted, 1,
        "the unlinked plan's unaddressed question is global in scoped attention"
    );
    let human = authority_actor("josh", "laptop", "human", "h1");
    assert_eq!(
        attention_ids(&attention_page(
            board.reader.as_ref().expect("read connection"),
            &human,
            None,
            true,
            200
        )),
        vec![id(5)]
    );
    let steward = authority_actor("josh", "laptop", "claude", "s2");
    let own = attention_page(
        board.reader.as_ref().expect("read connection"),
        &steward,
        None,
        true,
        200,
    );
    assert_eq!(
        attention_ids(&own),
        vec![id(4)],
        "the steward's own user-and-harness question leaves Needs you"
    );
    assert_eq!(own.rebase_needed, vec![id(4)]);
    let other_session = authority_actor("josh", "laptop", "claude", "s3");
    assert_eq!(
        attention_ids(&attention_page(
            board.reader.as_ref().expect("read connection"),
            &other_session,
            None,
            true,
            200
        )),
        vec![id(5)],
        "a sibling session did not ask the question"
    );
    let other_host = authority_actor("josh", "desktop", "codex", "s1");
    assert_eq!(
        attention_ids(&attention_page(
            board.reader.as_ref().expect("read connection"),
            &other_host,
            None,
            true,
            200
        )),
        vec![id(5)]
    );
}
