use super::*;
use crate::board::{
    board_actor::{BoardActor, HarnessLabel},
    board_backend::BoardBackend,
    board_ids::{EntryId, EventSeq},
    board_protocol::{
        BoardChange, BoardOp, BoardReceiptState, BoardReply, BoardRequest, BoardRequestKey,
        BoardResult,
    },
    local_board::LocalBoard,
};
use std::time::Duration;

#[test]
fn v12_adds_nullable_receipt_actor_without_rewriting_evidence() {
    let directory = directory();
    let path = directory.path().join("board.sqlite3");
    seed_storage_schema(&path, 11).unwrap();
    let key: BoardRequestKey = "22".repeat(32).parse().unwrap();
    let stored_reply = serde_json::to_string(&BoardReply {
        api: 11,
        backend: "local".into(),
        result: BoardResult::Change(BoardChange {
            entry: EntryId::new(1).unwrap(),
            seq: EventSeq::new(41),
            plan: None,
            revision: None,
            task: None,
            transferred_to: None,
            deduplicated: false,
        }),
        snapshot_seq: None,
        warnings: Vec::new(),
    })
    .unwrap();
    let legacy = Connection::open(&path).unwrap();
    legacy
        .execute_batch(
            "INSERT INTO actors(id,user,host,harness,session)
               VALUES(1,'legacy-user','legacy-host','codex','legacy-session');
             INSERT INTO entries(id,kind,body,actor_id,seq,created_at)
               VALUES(1,'feedback','report',1,41,100);
             INSERT INTO events(seq,kind,subject,actor_id,summary,created_at)
               VALUES(41,'feedback','E1',1,'report',100);
             INSERT INTO feedback_imports(import_key,entry_id) VALUES('permanent-import',1);",
        )
        .unwrap();
    legacy
        .execute(
            "INSERT INTO operation_dedupes(dedupe_key,reply_json,created_at)
             VALUES(?1,?2,?3)",
            rusqlite::params![key.to_string(), stored_reply, 100],
        )
        .unwrap();
    drop(legacy);

    let (migrated, _) = open(&path).unwrap();
    let version: i64 = migrated
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    assert_eq!(
        migrated
            .query_row("SELECT MAX(seq) FROM events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        41
    );
    assert_eq!(
        migrated
            .query_row("SELECT COUNT(*) FROM entries", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let retained: (String, i64, Option<i64>) = migrated
        .query_row(
            "SELECT reply_json,created_at,actor_id FROM operation_dedupes WHERE dedupe_key=?1",
            [key.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(retained, (stored_reply, 100, None));
    assert_eq!(
        migrated
            .query_row(
                "SELECT entry_id FROM feedback_imports WHERE import_key='permanent-import'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    drop(migrated);

    let actor = BoardActor::new(
        "legacy-user",
        "legacy-host",
        HarnessLabel::parse("codex").unwrap(),
        "legacy-session",
    )
    .unwrap();
    let mut board = LocalBoard::open_path(&path, Duration::from_secs(120)).unwrap();
    let reply = board
        .handle(&BoardRequest::new(actor, BoardOp::Receipt { key }))
        .unwrap();
    let BoardResult::Receipt(receipt) = reply.result else {
        panic!("expected receipt result");
    };
    assert_eq!(receipt.state, BoardReceiptState::Unknown);
    assert!(receipt.change.is_none());
}
