mod database_opening_tests;
mod manual_link_migration_tests;
mod repository_migration_tests;
mod schema_nine_tests;
mod schema_refusal_tests;
mod schema_repair_tests;

pub(super) use schema_refusal_tests::upgrade_schema_before_lock;

use super::*;

fn directory() -> tempfile::TempDir {
    crate::board::board_test_support::scratch("board-test-")
}

#[test]
fn shipped_migration_steps_are_byte_pinned() {
    use sha2::{Digest, Sha256};
    let pin = |sql: &str| {
        Sha256::digest(sql.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    // V1..V3 pins are the 715ce6c bytes; V4 is the W4-shipped step. Any edit,
    // whitespace included, must fail here and ship as a new step instead.
    assert_eq!(
        pin(SCHEMA_V1),
        "47413815133d1a963462fbfe86c3efe50886b2efffaa92083e891379b61824a7"
    );
    assert_eq!(
        pin(SCHEMA_V2),
        "030cc83ff064e0d7f21733a177031cfd20c79e56d800efa30b47bffe8d205345"
    );
    assert_eq!(
        pin(SCHEMA_V3),
        "89485ffc412986aa7b37a301194160d662386bcbae5fff7149abb5d013d8d982"
    );
    assert_eq!(
        pin(SCHEMA_V4),
        "3398d71703ed6b759c64530ef903da210e0ddf21c378a1daf4aea3ca89615c15"
    );
    assert_eq!(
        pin(SCHEMA_V5),
        "a4090c5ffac9c08f2ab5b85397cc97a03613ea1fcadc787dccb027cbad56c40e"
    );
    assert_eq!(
        pin(SCHEMA_V6),
        "83d284f13f2019c911ad4ded97fcb5ab05469ef89df071d72cc630864a9ec8ce"
    );
    assert_eq!(
        pin(SCHEMA_V7),
        "650667e03644b553c7712e329fd69433da899aedb20e16445178d7e275f97964"
    );
    assert_eq!(
        pin(SCHEMA_V8),
        "a182e6dbd24a6ff72524909ab87ee66abf64585afde1616f32cc75e8bd62d13d"
    );
    assert_eq!(
        pin(SCHEMA_V9),
        "9e78780cdd2f2cb7510f5d048982cf5026e5ca2ba626c9866ecd3631387e94d2"
    );
}

#[test]
fn populated_v1_migration_preserves_durable_evidence() {
    let directory = directory();
    let path = directory.path().join("board.sqlite3");
    let legacy = Connection::open(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    legacy.execute_batch(SCHEMA_V1).unwrap();
    legacy.execute_batch(r#"
    INSERT INTO actors VALUES(1,'josh','host','codex','session');
    INSERT INTO agent_sessions VALUES(1,'model','xhigh',7,1,10,20);
    INSERT INTO repos VALUES('repo','origin');
    INSERT INTO repo_paths VALUES('repo','host','/repo',NULL,'old-digest');
    INSERT INTO texts VALUES('body','original SSOT');
    INSERT INTO plans VALUES(1,'Plan','josh','codex',1,1,10);
    INSERT INTO plan_repos VALUES(1,'repo');
    INSERT INTO entries VALUES(1,1,'decision','created',NULL,NULL,1,'model','xhigh','repo',NULL,'old',1,10);
    INSERT INTO entries VALUES(2,1,'proposal','proposed',NULL,NULL,1,'model','xhigh','repo','open','old',2,11);
    INSERT INTO entries VALUES(3,1,'feedback','report',NULL,NULL,1,'model','xhigh','repo','open','old',3,12);
    INSERT INTO entry_refs VALUES(2,'P1@1');
    INSERT INTO revisions VALUES(1,1,'body','create',1,1,1);
    INSERT INTO proposals VALUES(2,1,1,'body','open',NULL,NULL);
    INSERT INTO tasks VALUES(1,1,'Task','doing','codex','Scope',1);
    INSERT INTO claims VALUES(1,1,1,1,1,'scope',10,20,NULL,NULL);
    INSERT INTO commits VALUES('repo','0123456789012345678901234567890123456789','commit',12,'author','[]',0,'[]',0,0);
    INSERT INTO commit_plans VALUES('repo','0123456789012345678901234567890123456789',1,1,1);
    INSERT INTO board_feedback VALUES(3,'wrong','1','build','cwd',NULL,'[]','import');
    INSERT INTO feedback_imports VALUES('import',3);
    INSERT INTO events VALUES(1,1,'decision','P1',NULL,1,'created',10);
    INSERT INTO events VALUES(2,1,'proposal','E2',NULL,1,'proposed',11);
    INSERT INTO operation_dedupes VALUES('operation','{"api":1,"backend":"legacy","result":{"result":"change",
 "data":{"entry":"E1","seq":1,"plan":"P1","revision":"P1@1","task":null,"deduplicated":false}},"warnings":[]}',20);
    PRAGMA user_version=1;
    "#).unwrap();
    drop(legacy);
    let (conn, _) = open(&path).unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    for (table, count) in [
        ("actors", 1),
        ("agent_sessions", 1),
        ("entries", 3),
        ("entry_refs", 1),
        ("revisions", 1),
        ("proposals", 1),
        ("tasks", 1),
        ("claims", 1),
        ("commits", 1),
        ("commit_plans", 1),
        ("commit_tasks", 1),
        ("board_feedback", 1),
        ("feedback_imports", 1),
        ("events", 2),
    ] {
        assert_eq!(
            conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            count,
            "{table}"
        );
    }
    assert_eq!(
        conn.query_row("SELECT body FROM texts", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "original SSOT"
    );
    assert_eq!(
        conn.query_row("SELECT task_ordinal FROM commit_tasks", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    for table in ["commit_plans", "commit_tasks"] {
        assert_eq!(
            conn.query_row(&format!("SELECT source FROM {table}"), [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            "scan",
            "{table} provenance backfills to scan"
        );
    }
    assert_eq!(
        conn.query_row("SELECT via FROM entries WHERE id=3", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "outbox"
    );
    assert_eq!(
        conn.query_row("SELECT model,effort FROM events WHERE seq=1", [], |row| Ok(
            (row.get::<_, String>(0)?, row.get::<_, String>(1)?)
        ))
        .unwrap(),
        ("model".into(), "xhigh".into())
    );
    assert_eq!(
        conn.query_row("SELECT delegated_by FROM claims WHERE id=1", [], |row| {
            row.get::<_, Option<i64>>(0)
        })
        .unwrap(),
        None
    );
    assert_eq!(
        conn.query_row(
            "SELECT cursor_seq,first_seen,last_seen FROM agent_sessions",
            [],
            |row| Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?
            ))
        )
        .unwrap(),
        (7, 10, 20)
    );
    let receipt: crate::board::board_protocol::BoardReply = serde_json::from_str(
        &conn
            .query_row("SELECT reply_json FROM operation_dedupes", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
    )
    .unwrap();
    // The v4 step stamps the wire API current at migration time; dispatch
    // upgrades older stamps to the current API when replaying receipts.
    assert_eq!(receipt.api, 3);
    let crate::board::board_protocol::BoardResult::Change(change) = receipt.result else {
        panic!("lost legacy receipt");
    };
    assert_eq!(change.entry.get(), 1);
    assert_eq!(change.seq.get(), 1);
    assert_eq!(change.revision.unwrap().to_string(), "P1@1");
    assert!(
        conn.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
    conn.execute(
        "UPDATE proposals SET state='superseded' WHERE entry_id=2",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE claims SET ended_at=30,end_reason='resumed' WHERE id=1",
        [],
    )
    .unwrap();
    drop(conn);
    let (conn, _) = open(&path).unwrap();
    assert_eq!(
        conn.query_row("SELECT state FROM proposals", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "superseded"
    );
    assert!(
        conn.prepare("SELECT bound_plan FROM agent_sessions")
            .is_err()
    );
    assert!(conn.prepare("SELECT dedupe_key FROM entries").is_err());
    assert!(conn.prepare("SELECT tips_digest FROM repo_paths").is_err());
}
