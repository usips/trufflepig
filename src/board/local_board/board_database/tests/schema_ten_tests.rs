use super::*;

#[test]
fn populated_v9_upgrade_preserves_history_and_migrates_legacy_steward() {
    let directory = directory();
    let path = directory.path().join("board.sqlite3");
    seed_storage_schema(&path, 9).unwrap();
    let legacy = Connection::open(&path).unwrap();
    legacy
        .execute_batch(
            "INSERT INTO actors VALUES(1,'owner','host','human','owner-session');
             INSERT INTO actors VALUES(2,'owner','host','codex','agent-session');
             INSERT INTO actors VALUES(3,'other','host','codex','other-session');
             INSERT INTO repos VALUES('repo','origin');
             INSERT INTO plans VALUES(1,'Plan','owner','codex',1,1,1);
             INSERT INTO plans VALUES(2,'Other plan','other',NULL,1,0,1);
             INSERT INTO tasks VALUES(1,1,'Task','doing','owner','Scope',7);
             INSERT INTO entries(id,plan_id,kind,body,actor_id,seq,created_at)
               VALUES(1,1,'claim','claimed',1,10,10);
             INSERT INTO entries(id,plan_id,kind,body,actor_id,seq,created_at)
               VALUES(2,1,'post','evidence',2,20,20);
             INSERT INTO entry_refs VALUES(2,'P1@1');
             INSERT INTO claims(id,plan_id,task_ordinal,actor_id,entry_id,scope,
               claimed_at,last_active,ended_at,end_reason,delegated_by)
               VALUES(1,1,1,1,1,'Scope',10,20,30,'resumed',NULL);
             INSERT INTO commits VALUES('repo','1111111111111111111111111111111111111111',
               'commit',25,'author','[]',1,1,0);
             INSERT INTO commit_plans(repo_key,oid,plan_id,entry_id,source)
               VALUES('repo','1111111111111111111111111111111111111111',1,2,'manual');
             INSERT INTO events(seq,plan_id,kind,subject,actor_id,summary,created_at)
               VALUES(20,1,'post','E2',2,'evidence',20);
             INSERT INTO events(seq,plan_id,kind,subject,to_whom,actor_id,summary,created_at)
               VALUES(40,1,'commit','E2','owner',1,'manual task link receipt',40);
             INSERT INTO commit_tasks(repo_key,oid,plan_id,task_ordinal,source,link_seq)
               VALUES('repo','1111111111111111111111111111111111111111',1,1,'manual',40);
             INSERT INTO agent_sessions(actor_id,model,effort,cursor_seq,first_seen,last_seen)
               VALUES(1,'model','high',37,10,20);
             INSERT INTO operation_dedupes VALUES('stale-api-9','{}',30);",
        )
        .unwrap();
    let before_refs = legacy
        .query_row(
            "SELECT target FROM entry_refs WHERE entry_id=2",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    let before_cursor = legacy
        .query_row("SELECT MAX(seq) FROM events", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    let before_receipt: (String, Option<i64>) = legacy
        .query_row(
            "SELECT source,link_seq FROM commit_tasks WHERE task_ordinal=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    drop(legacy);

    let (migrated, _) = open(&path).unwrap();
    assert_eq!(schema_version(&migrated), SCHEMA_VERSION);
    assert_eq!(
        migrated
            .query_row(
                "SELECT target FROM entry_refs WHERE entry_id=2",
                [],
                |row| { row.get::<_, String>(0) }
            )
            .unwrap(),
        before_refs
    );
    assert_eq!(
        migrated
            .query_row(
                "SELECT source,link_seq FROM commit_tasks WHERE task_ordinal=1",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .unwrap(),
        before_receipt
    );
    assert_eq!(
        migrated
            .query_row(
                "SELECT cursor_seq FROM agent_sessions WHERE actor_id=1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        37
    );
    assert_eq!(
        migrated
            .query_row("SELECT summary FROM events WHERE seq=40", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
        "manual task link receipt"
    );
    assert_eq!(
        migrated
            .query_row(
                "SELECT actor_id,origin_harness,origin_session FROM entries WHERE id=2",
                [],
                |row| Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?
                )),
            )
            .unwrap(),
        (2, None, None)
    );
    assert_eq!(
        migrated
            .query_row("SELECT end_reason FROM claims WHERE id=1", [], |row| {
                let reason = row.get::<_, String>(0)?;
                Ok(reason)
            },)
            .unwrap(),
        "resumed"
    );
    migrated
        .execute(
            "UPDATE claims SET end_reason='session_ended' WHERE id=1",
            [],
        )
        .unwrap();
    assert_eq!(
        migrated
            .query_row(
                "SELECT ended_at FROM agent_sessions WHERE actor_id=1",
                [],
                |row| row.get::<_, Option<i64>>(0),
            )
            .unwrap(),
        None
    );

    let grant: (i64, String, String, String, Option<i64>, String) = migrated
        .query_row(
            "SELECT grant_row.plan_id,grant_row.owner_user,grant_row.grantee,
                    grant_row.roles_json,grant_row.expires_at,grant_row.source
             FROM authority_grants AS grant_row",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        grant,
        (
            1,
            String::from("owner"),
            String::from("codex"),
            String::from("[\"triage\",\"release\",\"link\",\"assign\",\"decide\"]"),
            None,
            String::from("migration")
        )
    );
    assert_eq!(
        migrated
            .query_row(
                "SELECT COUNT(*) FROM authority_grants g JOIN actors a
                 ON a.user=g.owner_user AND a.harness=g.grantee
                 WHERE g.plan_id=1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1,
        "legacy steward grants remain constrained to the plan owner"
    );
    let (grant_entry, grant_seq, grant_actor, grant_body, grant_basis, grant_authority_entry): (
        i64,
        i64,
        String,
        String,
        String,
        Option<i64>,
    ) = migrated
        .query_row(
            "SELECT e.id,e.seq,a.harness,e.body,e.authority_basis,e.authority_entry
             FROM authority_grants g JOIN entries e ON e.id=g.entry_id
             JOIN actors a ON a.id=e.actor_id",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(grant_actor, "system");
    assert!(grant_body.starts_with("System migration:"));
    assert_eq!(grant_basis, "owner");
    assert_eq!(grant_authority_entry, None);
    assert!(grant_seq > before_cursor);
    assert!(
        migrated
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM events WHERE seq=?1 AND kind='grant'
                 AND subject='E'||?2 AND authority_basis='owner' AND authority_entry IS NULL)",
                rusqlite::params![grant_seq, grant_entry],
                |row| row.get::<_, bool>(0),
            )
            .unwrap()
    );
    assert_eq!(
        migrated
            .query_row("SELECT COUNT(*) FROM operation_dedupes", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
    assert!(!column_exists(&migrated, "plans", "steward"));
    assert_eq!(
        migrated
            .query_row(
                "SELECT worker_refs_json FROM commits WHERE oid='1111111111111111111111111111111111111111'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "[]"
    );
    assert_integrity(&migrated);
}

fn schema_version(connection: &Connection) -> i64 {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

fn column_exists(connection: &Connection, table: &str, column: &str) -> bool {
    let columns = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    columns.iter().any(|name| name == column)
}

fn assert_integrity(connection: &Connection) {
    assert_eq!(
        connection
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        connection
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
}
