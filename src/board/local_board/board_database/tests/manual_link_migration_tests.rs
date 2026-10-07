use super::*;
use std::path::Path;

const KNOWN_OID: &str = "1111111111111111111111111111111111111111";
const UNKNOWN_OID: &str = "2222222222222222222222222222222222222222";
const SCAN_OID: &str = "3333333333333333333333333333333333333333";

#[test]
fn schema_eight_backfills_only_exact_earliest_manual_events() {
    let directory = directory();
    let path = directory.path().join("board.sqlite3");
    seed_manual_link_history(&path);

    let (connection, _) = open(&path).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    let links = connection
        .prepare("SELECT task_ordinal,source,link_seq FROM commit_tasks ORDER BY task_ordinal")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        links,
        vec![
            (1, "manual".into(), Some(150)),
            (2, "manual".into(), None),
            (3, "scan".into(), None),
        ]
    );

    let foreign_keys = connection
        .prepare("PRAGMA foreign_key_list(commit_tasks)")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        foreign_keys
            .iter()
            .any(|link| { link == &("events".into(), "link_seq".into(), "seq".into()) })
    );
    assert!(
        connection
            .execute(
                "UPDATE commit_tasks SET link_seq=999 WHERE task_ordinal=2",
                [],
            )
            .is_err()
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

#[test]
fn failed_schema_eight_migration_rolls_back_and_reopens_after_repair() {
    let directory = directory();
    let path = directory.path().join("board.sqlite3");
    seed_storage_schema(&path, 7).unwrap();
    let legacy = Connection::open(&path).unwrap();
    legacy
        .execute(
            "INSERT INTO board_meta(key,value) VALUES('board_uuid','existing-board-uuid')",
            [],
        )
        .unwrap();
    legacy
        .execute_batch("CREATE INDEX events_manual_commit_lookup ON events(seq);")
        .unwrap();
    drop(legacy);

    let error = open(&path)
        .err()
        .expect("conflicting index must reject migration");
    assert!(error.to_string().contains("events_manual_commit_lookup"));
    let rolled_back = Connection::open(&path).unwrap();
    let version: i64 = rolled_back
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 7);
    assert!(!has_link_seq(&rolled_back));
    assert_eq!(
        rolled_back
            .query_row(
                "SELECT value FROM board_meta WHERE key='board_uuid'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "existing-board-uuid"
    );
    drop(rolled_back);

    let repair = Connection::open(&path).unwrap();
    repair
        .execute_batch("DROP INDEX events_manual_commit_lookup;")
        .unwrap();
    drop(repair);

    let (migrated, _) = open(&path).unwrap();
    assert!(has_link_seq(&migrated));
    let board_uuid: String = migrated
        .query_row(
            "SELECT value FROM board_meta WHERE key='board_uuid'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(board_uuid, "existing-board-uuid");
    drop(migrated);

    let (reopened, _) = open(&path).unwrap();
    let reopened_version: i64 = reopened
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(reopened_version, SCHEMA_VERSION);
    assert!(has_link_seq(&reopened));
    assert_eq!(
        reopened
            .query_row(
                "SELECT value FROM board_meta WHERE key='board_uuid'",
                [],
                |row| { row.get::<_, String>(0) }
            )
            .unwrap(),
        board_uuid
    );
}

pub(super) fn seed_manual_link_history(path: &Path) {
    seed_storage_schema(path, 7).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "INSERT INTO actors VALUES(1,'owner','host','cli','session');
             INSERT INTO repos VALUES('repo','origin');
             INSERT INTO plans VALUES(1,'Plan','owner',NULL,1,3,1);
             INSERT INTO plans VALUES(2,'Other plan','owner',NULL,1,0,1);",
        )
        .unwrap();

    for (task, entry, oid, source) in [
        (1_i64, 10_i64, KNOWN_OID, "manual"),
        (2, 20, UNKNOWN_OID, "manual"),
        (3, 30, SCAN_OID, "scan"),
    ] {
        connection
            .execute(
                "INSERT INTO tasks VALUES(1,?1,'Task','done',NULL,NULL,?1)",
                [task],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO entries(id,plan_id,kind,body,actor_id,seq,created_at)
                 VALUES(?1,1,'commit','commit evidence',1,?2,?2)",
                rusqlite::params![entry, entry],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO commits(repo_key,oid,subject,committed_at,author,coauthors,
                 files,insertions,deletions) VALUES('repo',?1,'subject',1,'author','[]',0,0,0)",
                [oid],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO commit_plans(repo_key,oid,plan_id,entry_id) VALUES('repo',?1,1,?2)",
                rusqlite::params![oid, entry],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO commit_tasks(repo_key,oid,plan_id,task_ordinal,source)
                 VALUES('repo',?1,1,?2,?3)",
                rusqlite::params![oid, task, source],
            )
            .unwrap();
    }

    let known_summary = format!("linked {KNOWN_OID} to P1.1 by hand");
    insert_event(&connection, 1, 200, "commit", "E10", &known_summary);
    insert_event(&connection, 1, 150, "commit", "E10", &known_summary);

    let unknown_summary = format!("linked {UNKNOWN_OID} to P1.2 by hand");
    insert_event(
        &connection,
        1,
        220,
        "commit",
        "E20",
        &format!("{unknown_summary} "),
    );
    insert_event(&connection, 1, 221, "commit", "E999", &unknown_summary);
    insert_event(&connection, 2, 222, "commit", "E20", &unknown_summary);
    insert_event(&connection, 1, 223, "note", "E20", &unknown_summary);

    let scan_summary = format!("linked {SCAN_OID} to P1.3 by hand");
    insert_event(&connection, 1, 230, "commit", "E30", &scan_summary);
}

fn insert_event(
    connection: &Connection,
    plan: i64,
    seq: i64,
    kind: &str,
    subject: &str,
    summary: &str,
) {
    connection
        .execute(
            "INSERT INTO events(seq,plan_id,kind,subject,actor_id,summary,created_at)
             VALUES(?1,?2,?3,?4,1,?5,?1)",
            rusqlite::params![seq, plan, kind, subject, summary],
        )
        .unwrap();
}

fn has_link_seq(connection: &Connection) -> bool {
    connection
        .prepare("PRAGMA table_info(commit_tasks)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .iter()
        .any(|column| column == "link_seq")
}
