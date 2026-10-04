use super::*;

#[test]
fn intermediate_v2_databases_gain_the_repair_indexes_without_losing_data() {
    let directory = directory();
    let path = directory.path().join("board.sqlite3");
    let legacy = Connection::open(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    // The v1 through v3 steps exactly as shipped, minus the two indexes the
    // v2 step gained only after some databases had already migrated.
    legacy.execute_batch(SCHEMA_V1).unwrap();
    legacy.execute_batch(SCHEMA_V2).unwrap();
    legacy.execute_batch(SCHEMA_V3).unwrap();
    legacy
        .execute_batch("DROP INDEX commit_plans_entry; DROP INDEX claims_entry_active;")
        .unwrap();
    legacy.execute_batch(r#"
    INSERT INTO actors VALUES(1,'josh','host','codex','session');
    INSERT INTO plans(id,title,owner_user,steward,head_revision,next_task,created_at) VALUES(1,'Plan','josh','codex',1,1,10);
    INSERT INTO repos VALUES('repo','origin');
    INSERT INTO entries(id,plan_id,kind,body,actor_id,seq,created_at) VALUES(1,1,'create','created',1,1,10);
    INSERT INTO tasks VALUES(1,1,'Task','doing','codex','Scope',1);
    INSERT INTO claims VALUES(1,1,1,1,1,'scope',10,20,NULL,NULL);
    INSERT INTO commits(repo_key,oid,subject,committed_at,author,coauthors,files,insertions,deletions) VALUES('repo','0123456789012345678901234567890123456789','commit',12,'author','[]',0,0,0);
    INSERT INTO commit_plans VALUES('repo','0123456789012345678901234567890123456789',1,1);
    PRAGMA user_version=3;
    "#).unwrap();
    drop(legacy);
    let (conn, _) = open(&path).unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    for index in ["commit_plans_entry", "claims_entry_active"] {
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='index' AND name=?1",
                [index],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1,
            "{index} must be repaired"
        );
    }
    assert_eq!(
        conn.query_row(
            "SELECT scope FROM claims WHERE id=1 AND ended_at IS NULL",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "scope"
    );
    assert_eq!(
        conn.query_row("SELECT entry_id FROM commit_plans", [], |row| row
            .get::<_, i64>(0))
        .unwrap(),
        1
    );
    assert!(
        conn.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
    drop(conn);
    let (conn, _) = open(&path).unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
}
