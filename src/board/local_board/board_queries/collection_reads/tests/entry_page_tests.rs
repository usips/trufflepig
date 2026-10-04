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
    let first = entries(&tx, None, None, 2);
    assert_eq!(
        first
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![id(1), id(3)]
    );
    assert_eq!(
        first.next_after,
        Some(EntryCursor {
            seq: EventSeq::new(3),
            entry: id(3)
        })
    );
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
    let second = entries(&tx, first.next_after, Some(first.through), 2);
    assert_eq!(
        second
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![id(4), id(5)]
    );
    assert_eq!(second.next_after, None);
    assert_eq!(max_seq(&tx).unwrap(), EventSeq::new(4));
    tx.commit().unwrap();
    let later = entries(
        board.reader.as_ref().expect("read connection"),
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
fn collection_entry_filters_match_all_actor_fields_and_task_links() {
    let (_directory, board) = database();
    seed_task(&board.conn, 1);
    seed_entry(&board.conn, 3, 3, 1, "note", 1, None, "matching");
    seed_entry(&board.conn, 4, 4, 1, "note", 2, None, "different host");
    seed_entry(&board.conn, 5, 5, 1, "note", 4, None, "different user");
    seed_entry(&board.conn, 6, 6, 1, "note", 3, None, "different harness");
    seed_entry(&board.conn, 7, 7, 1, "progress", 1, None, "different kind");
    seed_entry(&board.conn, 8, 8, 1, "note", 1, None, "different task");
    board
        .conn
        .execute_batch(
            "INSERT INTO entry_refs VALUES(3,'P1.1'),(4,'P1.1'),(5,'P1.1'),(6,'P1.1'),(7,'P1.1');",
        )
        .unwrap();
    let harness = HarnessLabel::parse("codex").unwrap();
    let task = TaskId::new(plan(1), 1).unwrap();
    let BoardResult::Entries(page) = entries_page(
        board.reader.as_ref().expect("read connection"),
        Some(plan(1)),
        Some(EntryKind::Note),
        Some(&harness),
        Some("josh"),
        Some("laptop"),
        Some(task),
        None,
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
        page.entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![id(3)]
    );
    let repo = register_repositories(&board.conn);
    seed_entry(&board.conn, 9, 9, 1, "commit", 1, None, "linked commit");
    let oid = "c".repeat(40);
    board
        .conn
        .execute(
            concat!(
                "INSERT INTO commits(repo_key,oid,subject,committed_at,author,coauthors,files,insertions,",
                "deletions) VALUES(?1,?2,'subject',1,'author','[]',1,1,1)"
            ),
            params![repo.as_str(), oid],
        )
        .unwrap();
    board
        .conn
        .execute(
            "INSERT INTO commit_plans VALUES(?1,?2,1,9)",
            params![repo.as_str(), oid],
        )
        .unwrap();
    board
        .conn
        .execute(
            "INSERT INTO commit_tasks VALUES(?1,?2,1,1)",
            params![repo.as_str(), oid],
        )
        .unwrap();
    let BoardResult::Entries(page) = entries_page(
        board.reader.as_ref().expect("read connection"),
        None,
        Some(EntryKind::Commit),
        None,
        None,
        None,
        Some(task),
        None,
        None,
        None,
        200,
    )
    .unwrap()
    .result
    else {
        panic!("entries");
    };
    assert_eq!(page.entries[0].id, id(9));
    assert_eq!(
        entries_page(
            board.reader.as_ref().expect("read connection"),
            Some(plan(2)),
            None,
            None,
            None,
            None,
            Some(task),
            None,
            None,
            None,
            200
        )
        .unwrap_err()
        .code,
        BoardErrorCode::InvalidReference
    );
}
