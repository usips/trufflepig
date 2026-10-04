use super::*;

#[test]
fn permanent_alias_replay_survives_expired_content_dedupe() {
    let (_dir, mut board) = board();
    let request = report("original");
    let first = changed(board.handle(&request).unwrap());
    let mut alias = request.clone();
    let BoardOp::Feedback { import_key, .. } = &mut alias.op else {
        unreachable!()
    };
    *import_key = Some(FeedbackImportKey::new());
    assert!(changed(board.handle(&alias).unwrap()).deduplicated);
    board
        .conn
        .execute("DELETE FROM operation_dedupes", [])
        .unwrap();
    let replayed = changed(board.handle(&alias).unwrap());
    assert!(replayed.deduplicated);
    assert_eq!(replayed.entry, first.entry);
    assert_eq!(board.max_seq().unwrap(), first.seq);
    assert_eq!(read_feedback(&board.conn, false).unwrap().len(), 1);
}

#[test]
fn feedback_content_dedupe_ignores_session_and_observation_metadata() {
    let (_dir, mut board) = board();
    let first = changed(board.handle(&report("original")).unwrap());
    let mut replay = report("restarted");
    let BoardOp::Feedback { metadata, .. } = &mut replay.op else {
        unreachable!()
    };
    metadata.cwd = "another/subdirectory".into();
    metadata.recent_calls.clear();
    metadata.steer_mode = Some("off".into());
    let replay = changed(board.handle(&replay).unwrap());
    assert!(replay.deduplicated);
    assert_eq!(replay.entry, first.entry);
    assert_eq!(board.max_seq().unwrap(), first.seq);
}

#[test]
fn populated_m1_feedback_uuid_replay_survives_schema_and_dedupe_upgrade() {
    let directory = crate::board::board_test_support::scratch("board-test-");
    let path = directory.path().join("legacy-feedback.sqlite3");
    let legacy = rusqlite::Connection::open(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    legacy
        .execute_batch(crate::board::local_board::board_database::legacy_schema())
        .unwrap();
    let key = FeedbackImportKey::parse("aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa").unwrap();
    legacy.execute_batch(r#"
        INSERT INTO actors VALUES(1,'josh','laptop','codex','original');
        INSERT INTO agent_sessions VALUES(1,NULL,NULL,NULL,NULL,10,10);
        INSERT INTO entries VALUES(1,NULL,'feedback','M1 report',NULL,NULL,1,NULL,NULL,NULL,
 'open','legacy-request-hash',1,10);
        INSERT INTO board_feedback VALUES(1,'wrong','0.1.0',NULL,'src',NULL,'[]',
 'aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa');
        INSERT INTO feedback_imports VALUES('aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa',1);
        INSERT INTO events VALUES(1,NULL,'feedback','E1',NULL,1,'M1 report',10);
        INSERT INTO operation_dedupes VALUES('legacy-request-hash',
 '{"api":1,"backend":"local","warnings":[],"result":{"result":"change",
 "data":{"entry":"E1","seq":1,"plan":null,"revision":null,"task":null,"deduplicated":false}}}',10);
        PRAGMA user_version=1;
    "#).unwrap();
    drop(legacy);
    let mut board = LocalBoard::open_path(&path, Duration::from_secs(1800)).unwrap();
    let request = BoardRequest::new(
        actor("restarted"),
        BoardOp::Feedback {
            kind: FeedbackKind::Wrong,
            summary: EntryText::new("M1 report").unwrap(),
            body: None,
            plan: None,
            metadata: FeedbackMetadata::default(),
            import_key: Some(key),
        },
    );
    let reply = board.handle(&request).unwrap();
    assert_eq!(reply.api, crate::board::board_protocol::BOARD_API);
    let replayed = changed(reply);
    assert!(replayed.deduplicated);
    assert_eq!(
        replayed.entry,
        crate::board::board_ids::EntryId::new(1).unwrap()
    );
    assert_eq!(replayed.seq.get(), 1);
    assert_eq!(board.max_seq().unwrap().get(), 1);
    assert_eq!(read_feedback(&board.conn, false).unwrap().len(), 1);
    let hashes: Vec<String> = board
        .conn
        .prepare("SELECT dedupe_key FROM operation_dedupes")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(hashes, ["legacy-request-hash"]);
}
