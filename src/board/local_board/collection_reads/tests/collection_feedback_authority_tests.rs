use super::*;

#[test]
fn collection_attention_shared_feedback_respects_management_authority_before_limit() {
    let (_directory, board) = database();
    let repo = register_repositories(&board.conn);
    for (entry, plan, reporter, to) in [
        (3, 1, 2, None),
        (4, 1, 4, None),
        (5, 1, 3, None),
        (6, 1, 4, None),
        (7, 2, 2, None),
        (8, 2, 1, Some("muse")),
        (9, 1, 1, None),
        (10, 2, 2, Some("codex")),
    ] {
        seed_entry(
            &board.conn,
            entry,
            entry,
            plan,
            "feedback",
            reporter,
            to,
            "report",
        );
    }
    board.conn.execute_batch("UPDATE entries SET state='open' WHERE kind='feedback'; UPDATE entries SET state='triaged' WHERE id=3; UPDATE entries SET state='fixed' WHERE id=9; UPDATE entries SET plan_id=NULL WHERE id IN (5,6); UPDATE plans SET owner_user='other' WHERE id=2;").unwrap();
    board
        .conn
        .execute("UPDATE entries SET repo_key=?1 WHERE id=5", [repo.as_str()])
        .unwrap();
    let ordinary = attention_page(
        board.reader.as_ref().expect("read connection"),
        &context(),
        None,
        true,
        1,
    );
    assert_eq!(attention_ids(&ordinary), vec![id(8)]);
    assert_eq!(ordinary.entries_omitted, 0);
    let human = authority_actor("josh", "laptop", "human", "h1");
    let first = attention_page(
        board.reader.as_ref().expect("read connection"),
        &human,
        None,
        true,
        1,
    );
    assert_eq!(attention_ids(&first), vec![id(3)]);
    assert_eq!(first.entries_omitted, 2);
    let BoardResult::Attention(rest) = attention(
        board.reader.as_ref().expect("read connection"),
        &human,
        None,
        true,
        first.next_after,
        Some(first.through),
        200,
    )
    .unwrap()
    .result
    else {
        panic!("attention");
    };
    assert_eq!(attention_ids(&rest), vec![id(4), id(5)]);
    assert_eq!(
        attention_ids(&attention_page(
            board.reader.as_ref().expect("read connection"),
            &human,
            Some(&repo),
            false,
            200
        )),
        vec![id(3), id(4), id(5)]
    );
    let steward = authority_actor("josh", "laptop", "claude", "s2");
    assert_eq!(
        attention_ids(&attention_page(
            board.reader.as_ref().expect("read connection"),
            &steward,
            None,
            true,
            200
        )),
        vec![id(3), id(4), id(5)]
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
        vec![id(6), id(7)]
    );
    let foreign_steward = authority_actor("other", "laptop", "claude", "s2");
    assert!(
        attention_page(
            board.reader.as_ref().expect("read connection"),
            &foreign_steward,
            None,
            true,
            200
        )
        .entries
        .is_empty()
    );
}
