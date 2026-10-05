use super::*;

#[test]
fn board_search_migrates_populated_v1_through_v2_v3_without_losing_rows() {
    let directory = crate::board::board_test_support::scratch("board-search-");
    let (path, before) = legacy_search_fixture::populated_v1(&directory);
    let mut board = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    assert_eq!(
        board
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        crate::board::SCHEMA_VERSION
    );
    assert_eq!(legacy_search_fixture::counts(&board.conn), before);
    assert_eq!(
        board
            .conn
            .query_row("SELECT count(*) FROM commit_tasks", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        board
            .conn
            .query_row("SELECT subject FROM commits", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "retained commit"
    );
    assert_eq!(
        board
            .conn
            .query_row("SELECT import_key FROM feedback_imports", [], |row| row
                .get::<_, String>(
                0
            ))
            .unwrap(),
        "retained import"
    );
    assert_eq!(
        board
            .conn
            .query_row("SELECT last_active FROM claims WHERE id=1", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        5
    );
    assert_eq!(
        board
            .conn
            .query_row("SELECT state FROM proposals WHERE entry_id=4", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
        "open"
    );
    assert_eq!(
        board
            .conn
            .query_row(
                "SELECT cursor_seq FROM agent_sessions WHERE actor_id=1",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    assert_eq!(
        targets(&results(&mut board, "migrationneedle", None, 50)),
        ["P1@1".to_owned(), "P2@1".to_owned()].into()
    );
    assert_eq!(
        targets(&results(&mut board, "proposalneedle", None, 50)),
        ["E4".to_owned()].into()
    );
    assert_eq!(
        targets(&results(&mut board, "summaryneedle", None, 50)),
        ["E4".to_owned()].into()
    );
    let global = results(&mut board, "globalneedle", None, 50);
    assert_eq!(targets(&global), ["E5".to_owned()].into());
    assert!(global["hits"][0]["plan"].is_null());
    assert!(
        targets(&results(
            &mut board,
            "globalneedle",
            Some(PlanId::new(1).unwrap()),
            50
        ))
        .is_empty()
    );
    assert_eq!(
        board
            .conn
            .query_row("SELECT count(*) FROM search_documents", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        7
    );
    let edited = call(
        &mut board,
        BoardOp::Edit {
            base: PlanRevision::new(PlanId::new(1).unwrap(), 1).unwrap(),
            body: PlanText::new("postmigrationneedle revision").unwrap(),
            summary: EntryText::new("after migration").unwrap(),
        },
    );
    assert_eq!(edited.revision.unwrap().revision, 2);
    assert_eq!(
        targets(&results(&mut board, "postmigrationneedle", None, 50)),
        ["P1@2".to_owned()].into()
    );
    integrity(&board);
    drop(board);
    let board = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    integrity(&board);
}

#[test]
fn board_search_v5_to_v6_migration_indexes_every_existing_title() {
    let directory = crate::board::board_test_support::scratch("board-search-");
    let path = directory.path().join("v5.sqlite3");
    // Seed the exact v5 bytes: v5 is the last step without title search.
    crate::board::local_board::seed_storage_schema(&path, 5).unwrap();
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        r#"
INSERT INTO actors(id,user,host,harness,session) VALUES(1,'josh','host','human','search-test');
INSERT INTO texts(hash,body) VALUES('hash','legacybody retained revision');
INSERT INTO plans(id,title,owner_user,head_revision,next_task,created_at)
 VALUES(1,'Alpine valve schedule','josh',1,0,1),(2,'Harbor manifest review','josh',1,0,2),
 (3,'Cinder audit trail','josh',1,0,3);
INSERT INTO revisions(plan_id,number,text_hash,source,entry_id,actor_id,seq)
 VALUES(1,1,'hash','create',NULL,1,1);
INSERT INTO entries(id,plan_id,kind,body,actor_id,seq,created_at)
 VALUES(1,2,'note','legacybody retained note',1,1,2);
"#,
    )
    .unwrap();
    drop(conn);
    let mut board = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    assert_eq!(
        board
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        crate::board::SCHEMA_VERSION
    );
    assert_eq!(
        board
            .conn
            .query_row("SELECT count(*) FROM plan_text", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        3
    );
    for (word, target) in [("alpine", "P1"), ("harbor", "P2"), ("cinder", "P3")] {
        let found = results(&mut board, word, None, 20);
        assert_eq!(targets(&found), [target.to_owned()].into(), "{word}");
        assert_eq!(found["hits"][0]["source"], "plan");
        assert_eq!(found["hits"][0]["plan"], target);
    }
    // Body search still covers pre-existing rows after the v6 backfill.
    assert_eq!(
        targets(&results(&mut board, "legacybody", None, 20)),
        ["P1@1".to_owned(), "E1".to_owned()].into()
    );
    integrity(&board);
}

#[test]
fn board_search_failed_v3_migration_rolls_back_rebuild_and_version_atomically() {
    let directory = crate::board::board_test_support::scratch("board-search-");
    let (path, before) = legacy_search_fixture::populated_v1(&directory);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        concat!(
            "CREATE TRIGGER fail_migration_meta BEFORE INSERT ON board_meta WHEN new.key='resolved_path' ",
            "BEGIN SELECT RAISE(ABORT,'injected migration failure'); END;"
        ),
    )
    .unwrap();
    drop(conn);
    let error = match LocalBoard::open_path(&path, Duration::from_secs(120)) {
        Err(error) => error,
        Ok(_) => panic!("accepted injected migration failure"),
    };
    assert_eq!(error.code, BoardErrorCode::InvalidState);
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(legacy_search_fixture::counts(&conn), before);
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name IN ('search_documents','board_text','plan_text','plan_titles')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );
    conn.execute_batch("DROP TRIGGER fail_migration_meta;")
        .unwrap();
    drop(conn);
    let board = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    assert_eq!(legacy_search_fixture::counts(&board.conn), before);
    integrity(&board);
}
