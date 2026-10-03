use super::*;

#[test]
fn collection_feed_and_revision_history_use_bounded_sequence_windows() {
    let (_directory, board) = database();
    seed_entry(&board.conn, 3, 3, 1, "note", 1, None, "discussion");
    seed_entry(&board.conn, 4, 4, 1, "direct", 3, None, "revision summary");
    board
        .conn
        .execute("INSERT INTO texts VALUES('large',?1)", ["x".repeat(16000)])
        .unwrap();
    board.conn.execute_batch("INSERT INTO revisions VALUES(1,2,'large','direct',4,3,4); UPDATE plans SET head_revision=2 WHERE id=1;").unwrap();
    let BoardResult::Feed(first) = feed(
        board.reader.as_ref().expect("read connection"),
        None,
        None,
        None,
        2,
    )
    .unwrap()
    .result
    else {
        panic!("feed");
    };
    assert_eq!(first.events.len(), 2);
    assert_eq!(first.next_after, Some(EventSeq::new(2)));
    assert_eq!(first.through, EventSeq::new(4));
    let BoardResult::Feed(second) = feed(
        board.reader.as_ref().expect("read connection"),
        Some(plan(1)),
        first.next_after,
        Some(first.through),
        500,
    )
    .unwrap()
    .result
    else {
        panic!("feed");
    };
    assert_eq!(
        second
            .events
            .iter()
            .map(|event| event.seq.get())
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    let BoardResult::History(first) = history(
        board.reader.as_ref().expect("read connection"),
        plan(1),
        None,
        None,
        1,
    )
    .unwrap()
    .result
    else {
        panic!("history");
    };
    assert_eq!(first.revisions.len(), 1);
    assert_eq!(first.revisions[0].summary.as_str(), "One");
    assert_eq!(first.next_after, Some(EventSeq::new(1)));
    let BoardResult::History(second) = history(
        board.reader.as_ref().expect("read connection"),
        plan(1),
        first.next_after,
        Some(first.through),
        1,
    )
    .unwrap()
    .result
    else {
        panic!("history");
    };
    assert_eq!(
        second.revisions[0].id,
        PlanRevision::new(plan(1), 2).unwrap()
    );
    assert_eq!(second.revisions[0].source, RevisionSource::Direct);
    assert_eq!(second.revisions[0].summary.as_str(), "revision summary");
    assert_eq!(second.next_after, None);
    let json = serde_json::to_value(second).unwrap();
    assert!(json["revisions"][0].get("body").is_none());
}

#[test]
fn collection_pages_reject_invalid_bounds_and_unknown_filters() {
    let (_directory, board) = database();
    assert_eq!(
        feed(
            board.reader.as_ref().expect("read connection"),
            None,
            None,
            None,
            501
        )
        .unwrap_err()
        .code,
        BoardErrorCode::InvalidOptions
    );
    assert_eq!(
        history(
            board.reader.as_ref().expect("read connection"),
            plan(1),
            None,
            None,
            201
        )
        .unwrap_err()
        .code,
        BoardErrorCode::InvalidOptions
    );
    assert_eq!(
        overview(
            board.reader.as_ref().expect("read connection"),
            &context(),
            None,
            None,
            None,
            0
        )
        .unwrap_err()
        .code,
        BoardErrorCode::InvalidOptions
    );
    assert_eq!(
        attention(
            board.reader.as_ref().expect("read connection"),
            &context(),
            None,
            true,
            None,
            None,
            201
        )
        .unwrap_err()
        .code,
        BoardErrorCode::InvalidOptions
    );
    assert_eq!(
        feed(
            board.reader.as_ref().expect("read connection"),
            None,
            None,
            Some(EventSeq::new(3)),
            1
        )
        .unwrap_err()
        .code,
        BoardErrorCode::InvalidReference
    );
    assert_eq!(
        feed(
            board.reader.as_ref().expect("read connection"),
            None,
            Some(EventSeq::new(2)),
            Some(EventSeq::new(1)),
            1
        )
        .unwrap_err()
        .code,
        BoardErrorCode::InvalidReference
    );
    assert_eq!(
        history(
            board.reader.as_ref().expect("read connection"),
            plan(99),
            None,
            None,
            1
        )
        .unwrap_err()
        .code,
        BoardErrorCode::InvalidReference
    );
    assert_eq!(
        entries_page(
            board.reader.as_ref().expect("read connection"),
            None,
            None,
            None,
            None,
            None,
            Some(TaskId::new(plan(1), 99).unwrap()),
            None,
            None,
            None,
            1
        )
        .unwrap_err()
        .code,
        BoardErrorCode::InvalidReference
    );
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
            Some(EntryCursor {
                seq: EventSeq::new(2),
                entry: id(2)
            }),
            Some(EventSeq::new(1)),
            1
        )
        .unwrap_err()
        .code,
        BoardErrorCode::InvalidReference
    );
}
