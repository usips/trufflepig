use super::*;

#[test]
fn collection_overview_pages_freeze_initial_revision_membership() {
    let (_directory, board) = database();
    let first = overview(board.reader.as_ref().expect("read connection"), None, None);
    assert_eq!(first.plans[0].plan.id, plan(1));
    assert_eq!(first.next_after, Some(plan(1)));
    assert_eq!(first.omitted, 1);
    board.conn.execute_batch("INSERT INTO plans(id,title,owner_user,head_revision,created_at) VALUES(3,'Three','josh',1,1);").unwrap();
    entry(&board.conn, 3, 3, "create");
    board.conn.execute_batch("UPDATE entries SET plan_id=3 WHERE id=3; INSERT INTO revisions VALUES(3,1,'one','create',3,1,3);").unwrap();
    let second = overview(
        board.reader.as_ref().expect("read connection"),
        first.next_after,
        Some(first.through),
    );
    assert_eq!(second.plans[0].plan.id, plan(2));
    assert_eq!(second.next_after, None);
    assert_eq!(second.omitted, 0);
    entry(&board.conn, 4, 4, "question");
    let frozen_first = overview(
        board.reader.as_ref().expect("read connection"),
        None,
        Some(first.through),
    );
    assert_eq!(frozen_first.plans[0].open_questions, 0);
}

#[test]
fn collection_attention_pages_preserve_siblings_and_claim_cursors() {
    let (_directory, board) = database();
    entry(&board.conn, 3, 3, "question");
    entry(&board.conn, 4, 3, "question");
    entry(&board.conn, 5, 4, "question");
    task(&board.conn, 1, 1);
    task(&board.conn, 2, 1);
    claim(&board.conn, 1, 1, 1, 1, 79);
    claim(&board.conn, 2, 2, 1, 1, 79);
    let BoardResult::Attention(first) = collection_reads::attention(
        board.reader.as_ref().expect("read connection"),
        &context(),
        None,
        true,
        None,
        None,
        1,
    )
    .unwrap()
    .result
    else {
        panic!("attention");
    };
    assert_eq!(first.entries[0].id, id(3));
    assert_eq!(
        first.claims_next_after,
        Some(ClaimCursor {
            entry: id(1),
            claim: 1
        })
    );
    entry(&board.conn, 6, 5, "question");
    let BoardResult::Attention(second) = collection_reads::attention(
        board.reader.as_ref().expect("read connection"),
        &context(),
        None,
        true,
        first.next_after,
        Some(first.through),
        1,
    )
    .unwrap()
    .result
    else {
        panic!("attention");
    };
    assert_eq!(second.entries[0].id, id(4));
    let BoardResult::Attention(third) = collection_reads::attention(
        board.reader.as_ref().expect("read connection"),
        &context(),
        None,
        true,
        second.next_after,
        Some(first.through),
        1,
    )
    .unwrap()
    .result
    else {
        panic!("attention");
    };
    assert_eq!(third.entries[0].id, id(5));
    assert_eq!(third.next_after, None);
    assert_eq!(third.entries_omitted, 0);
    let claims = claim_window(
        board.reader.as_ref().expect("read connection"),
        &context(),
        None,
        true,
        None,
        true,
        first.claims_next_after,
        Some(first.through),
        1,
    )
    .unwrap();
    assert_eq!(claims.claims[0].cursor.claim, 2);
}
