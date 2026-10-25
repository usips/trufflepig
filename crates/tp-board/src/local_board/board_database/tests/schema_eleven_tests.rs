use super::*;

#[test]
fn populated_v10_upgrade_preserves_origins_and_adds_null_peer_addresses() {
    let directory = directory();
    let path = directory.path().join("board.sqlite3");
    seed_storage_schema(&path, 10).unwrap();
    let legacy = Connection::open(&path).unwrap();
    legacy
        .execute_batch(
            "INSERT INTO actors VALUES(1,'user','host','codex','session');
             INSERT INTO entries(id,kind,body,actor_id,seq,created_at,origin_harness,origin_session)
               VALUES(1,'note','entry body',1,10,10,'codex','client-session');
             INSERT INTO events(seq,kind,subject,actor_id,summary,created_at,origin_harness,origin_session)
               VALUES(10,'note','P1',1,'event summary',10,'codex','client-session');
             INSERT INTO operation_dedupes VALUES('stale-api-10','{}',10);",
        )
        .unwrap();
    drop(legacy);

    let (migrated, _) = open(&path).unwrap();
    assert_eq!(
        migrated
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        SCHEMA_VERSION
    );
    let entry_origin: (Option<String>, Option<String>, Option<String>) = migrated
        .query_row(
            "SELECT origin_harness,origin_session,origin_ip FROM entries WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let event_origin: (Option<String>, Option<String>, Option<String>) = migrated
        .query_row(
            "SELECT origin_harness,origin_session,origin_ip FROM events WHERE seq=10",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        entry_origin,
        (Some("codex".into()), Some("client-session".into()), None)
    );
    assert_eq!(event_origin, entry_origin);
    assert_eq!(
        migrated
            .query_row("SELECT COUNT(*) FROM operation_dedupes", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}
