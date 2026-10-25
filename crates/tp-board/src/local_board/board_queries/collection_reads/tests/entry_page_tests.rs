use super::*;

#[test]
fn collection_entries_preserve_shared_sequences_and_frozen_snapshot() {
    let (_directory, mut board) = database();
    seed_entry(&board.conn, 3, 3, 1, "note", 1, None, "first sibling");
    seed_entry(&board.conn, 4, 3, 1, "note", 1, None, "second sibling");
    seed_entry(&board.conn, 5, 4, 1, "note", 1, None, "next sequence");
    let tx = board
        .reader
        .as_mut()
        .expect("read connection")
        .transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)
        .unwrap();
    let first = entries(&tx, None, None, None, 2);
    assert_eq!(
        first
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![id(5), id(4)]
    );
    assert_eq!(
        first.next_before,
        Some(EntryCursor {
            seq: EventSeq::new(3),
            entry: id(4)
        })
    );
    assert!(first.next_after.is_none());
    assert_eq!(first.through, EventSeq::new(4));
    seed_entry(
        &board.conn,
        6,
        5,
        1,
        "note",
        1,
        None,
        "concurrent later write",
    );
    let second = entries(&tx, None, first.next_before, Some(first.through), 2);
    assert_eq!(
        second
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![id(3), id(1)]
    );
    assert_eq!(second.next_before, None);
    assert_eq!(max_seq(&tx).unwrap(), EventSeq::new(4));
    tx.commit().unwrap();
    let later = entries(
        board.reader.as_ref().expect("read connection"),
        None,
        second.entries.last().map(|entry| EntryCursor {
            seq: entry.seq,
            entry: entry.id,
        }),
        Some(first.through),
        2,
    );
    assert!(later.entries.is_empty());
    assert_eq!(
        max_seq(board.reader.as_ref().expect("read connection")).unwrap(),
        EventSeq::new(5)
    );
    let session: (i64, i64) = board
        .conn
        .query_row(
            "SELECT cursor_seq,last_seen FROM agent_sessions WHERE actor_id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(session, (17, 1));
}

#[test]
fn collection_entries_page_ascending_behind_an_after_cursor() {
    let (_directory, board) = database();
    seed_entry(&board.conn, 3, 3, 1, "note", 1, None, "first sibling");
    seed_entry(&board.conn, 4, 3, 1, "note", 1, None, "second sibling");
    seed_entry(&board.conn, 5, 4, 1, "note", 1, None, "next sequence");
    let reader = board.reader.as_ref().expect("read connection");
    let start = EntryCursor {
        seq: EventSeq::new(0),
        entry: id(1),
    };
    let first = entries(reader, Some(start), None, None, 2);
    assert_eq!(
        first
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![id(1), id(3)]
    );
    assert_eq!(first.after, Some(start));
    assert_eq!(
        first.next_after,
        Some(EntryCursor {
            seq: EventSeq::new(3),
            entry: id(3)
        })
    );
    assert!(first.next_before.is_none());
    let second = entries(reader, first.next_after, None, Some(first.through), 2);
    assert_eq!(
        second
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![id(4), id(5)]
    );
    assert_eq!(second.next_after, None);
    assert!(second.next_before.is_none());
}

#[test]
fn collection_entries_reject_combined_after_and_before_cursors() {
    let (_directory, board) = database();
    let cursor = EntryCursor {
        seq: EventSeq::new(1),
        entry: id(1),
    };
    assert_eq!(
        entries_page(
            board.reader.as_ref().expect("read connection"),
            Some(plan(1)),
            None,
            None,
            None,
            None,
            None,
            None,
            Some(cursor),
            Some(cursor),
            None,
            50,
        )
        .unwrap_err()
        .code,
        BoardErrorCode::InvalidOptions
    );
}
