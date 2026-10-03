use super::*;

#[test]
fn collection_reference_pages_filter_replies_and_paginate_backrefs() {
    let (_directory, board) = database();
    entry(&board.conn, 3, 3, "question");
    entry(&board.conn, 4, 4, "answer");
    entry(&board.conn, 5, 4, "note");
    entry(&board.conn, 6, 4, "answer");
    board
        .conn
        .execute_batch("INSERT INTO entry_refs VALUES(4,'E3'),(5,'E3'),(6,'E3');")
        .unwrap();
    let BoardResult::Entries(first) = collection_reads::entries_page(
        board.reader.as_ref().expect("read connection"),
        None,
        Some(EntryKind::Answer),
        None,
        None,
        None,
        None,
        Some(id(3)),
        None,
        None,
        1,
    )
    .unwrap()
    .result
    else {
        panic!("entries");
    };
    assert_eq!(first.entries[0].id, id(4));
    assert_eq!(first.references, Some(id(3)));
    let BoardResult::Entries(second) = collection_reads::entries_page(
        board.reader.as_ref().expect("read connection"),
        None,
        Some(EntryKind::Answer),
        None,
        None,
        None,
        None,
        Some(id(3)),
        first.next_after,
        Some(first.through),
        1,
    )
    .unwrap()
    .result
    else {
        panic!("entries");
    };
    assert_eq!(second.entries[0].id, id(6));
    assert_eq!(second.next_after, None);
    let BoardResult::Entries(backrefs) = collection_reads::entries_page(
        board.reader.as_ref().expect("read connection"),
        None,
        None,
        None,
        None,
        None,
        None,
        Some(id(3)),
        None,
        None,
        200,
    )
    .unwrap()
    .result
    else {
        panic!("entries");
    };
    assert_eq!(
        backrefs
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![id(4), id(5), id(6)]
    );
    assert_eq!(
        first.next_after,
        Some(EntryCursor {
            seq: EventSeq::new(4),
            entry: id(4)
        })
    );
}
