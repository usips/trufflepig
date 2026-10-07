use super::*;

#[test]
fn v9_drops_backfill_index() {
    let directory = directory();
    let path = directory.path().join("board.sqlite3");
    manual_link_migration_tests::seed_manual_link_history(&path);
    let legacy = Connection::open(&path).unwrap();
    legacy.execute_batch(SCHEMA_V8).unwrap();
    legacy.pragma_update(None, "user_version", 8).unwrap();
    legacy
        .execute(
            "INSERT INTO board_meta(key,value) VALUES('board_uuid','durable-v8-board')",
            [],
        )
        .unwrap();
    let resolved_path = path.canonicalize().unwrap();
    legacy
        .execute(
            "INSERT INTO board_meta(key,value) VALUES('resolved_path',?1)",
            [resolved_path.to_string_lossy().as_ref()],
        )
        .unwrap();
    assert!(backfill_index_exists(&legacy));
    let schema_before = durable_schema_objects(&legacy);
    let evidence_before = DurableLinkSnapshot::read(&legacy);
    drop(legacy);

    let (migrated, _) = open(&path).unwrap();
    assert!(
        !backfill_index_exists(&migrated),
        "schema v9 must drop the backfill-only events_manual_commit_lookup index"
    );
    assert_eq!(schema_version(&migrated), 9);
    assert_eq!(durable_schema_objects(&migrated), schema_before);
    assert_eq!(DurableLinkSnapshot::read(&migrated), evidence_before);
    assert_integrity(&migrated);
    drop(migrated);

    let (reopened, _) = open(&path).unwrap();
    assert_eq!(schema_version(&reopened), 9);
    assert!(!backfill_index_exists(&reopened));
    assert_eq!(durable_schema_objects(&reopened), schema_before);
    assert_eq!(DurableLinkSnapshot::read(&reopened), evidence_before);
    assert_integrity(&reopened);
}

#[test]
fn fresh_schema_nine_keeps_durable_indexes_and_integrity() {
    let directory = directory();
    let path = directory.path().join("board.sqlite3");
    let (connection, _) = open(&path).unwrap();
    assert!(
        !backfill_index_exists(&connection),
        "fresh databases must not retain the manual-link backfill index"
    );
    assert_eq!(schema_version(&connection), 9);
    assert_eq!(SCHEMA_VERSION, 9);
    let indexes = connection
        .prepare("SELECT name FROM sqlite_schema WHERE type='index' AND tbl_name='events'")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(indexes, vec![String::from("events_plan_sequence")]);
    assert_integrity(&connection);
}

fn schema_version(connection: &Connection) -> i64 {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

fn backfill_index_exists(connection: &Connection) -> bool {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema
             WHERE type='index' AND name='events_manual_commit_lookup')",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn durable_schema_objects(connection: &Connection) -> Vec<(String, String, String)> {
    connection
        .prepare(
            "SELECT type,name,coalesce(sql,'') FROM sqlite_schema
             WHERE name<>'events_manual_commit_lookup' ORDER BY type,name",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

#[derive(Debug, PartialEq, Eq)]
struct DurableLinkSnapshot(Vec<(String, String)>);

impl DurableLinkSnapshot {
    fn read(connection: &Connection) -> Self {
        let rows = connection
            .prepare(
                "SELECT 'link' AS kind,json_array(repo_key,oid,plan_id,task_ordinal,source,link_seq) AS evidence
                   FROM commit_tasks
                 UNION ALL SELECT 'event',json_array(seq,plan_id,kind,subject,to_whom,actor_id,summary,created_at,model,effort)
                   FROM events
                 UNION ALL SELECT 'commit',json_array(repo_key,oid,subject,committed_at,author,coauthors,files,insertions,deletions)
                   FROM commits
                 UNION ALL SELECT 'plan_link',json_array(repo_key,oid,plan_id,entry_id,source)
                   FROM commit_plans
                 UNION ALL SELECT 'entry',json_array(id,plan_id,kind,body,actor_id,seq,created_at)
                   FROM entries
                 UNION ALL SELECT 'meta',json_array(key,value) FROM board_meta
                 ORDER BY kind,evidence",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        Self(rows)
    }
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
    connection
        .execute_batch(
            "INSERT INTO board_text(board_text,rank) VALUES('integrity-check',1);
             INSERT INTO plan_titles(plan_titles,rank) VALUES('integrity-check',1);",
        )
        .unwrap();
}
