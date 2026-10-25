use super::*;

#[test]
fn v13_preserves_authority_receipts_and_initializes_open_plans() {
    let directory = directory();
    let path = directory.path().join("board.sqlite3");
    seed_storage_schema(&path, 12).unwrap();
    let legacy = Connection::open(&path).unwrap();
    legacy.execute_batch(
        "PRAGMA foreign_keys=ON;
         INSERT INTO actors(id,user,host,harness,session) VALUES(1,'owner','host','human','owner'),(2,'owner','host','codex','worker');
         INSERT INTO plans(id,title,owner_user,head_revision,created_at) VALUES(1,'Retained plan','owner',1,100);
         INSERT INTO entries(id,plan_id,kind,body,actor_id,seq,created_at,authority_basis,authority_entry)
          VALUES(1,1,'new','original evidence',1,1,100,'owner',NULL),
                (2,1,'grant','original grant',1,2,101,'owner',NULL),
                (3,1,'task','created P1.1: retained task',2,3,102,'grant',2);
         INSERT INTO events(seq,plan_id,kind,subject,actor_id,summary,created_at,authority_basis,authority_entry)
          SELECT seq,plan_id,kind,'E'||id,actor_id,body,created_at,authority_basis,authority_entry FROM entries;
         INSERT INTO tasks(plan_id,ordinal,title,column_name,seq) VALUES(1,1,'retained task','todo',3);
         INSERT INTO entry_refs(entry_id,target) VALUES(3,'P1.1');
         INSERT INTO operation_dedupes(dedupe_key,reply_json,created_at,actor_id) VALUES('retained-request','{\"retained\":true}',102,2);"
    ).unwrap();
    let before_entries = evidence(&legacy, "entries", "id");
    let before_events = evidence(&legacy, "events", "seq");
    drop(legacy);

    let (migrated, _) = open(&path).unwrap();
    assert_eq!(evidence(&migrated, "entries", "id"), before_entries);
    assert_eq!(evidence(&migrated, "events", "seq"), before_events);
    assert_eq!(
        migrated
            .query_row("SELECT lifecycle_state FROM plans WHERE id=1", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
        "open"
    );
    assert_eq!(migrated.query_row("SELECT reply_json,actor_id FROM operation_dedupes WHERE dedupe_key='retained-request'", [], |row| Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?))).unwrap(), ("{\"retained\":true}".into(),2));
    assert_eq!(
        migrated
            .query_row("SELECT COUNT(*) FROM task_transfers", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    migrated.execute("INSERT INTO entries(id,plan_id,kind,body,actor_id,seq,created_at,authority_basis) VALUES(4,1,'task','P1.1 -> blocked',2,4,103,'creator')",[]).unwrap();
    assert_eq!(
        migrated
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        migrated
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
}

fn evidence(
    connection: &Connection,
    table: &str,
    key: &str,
) -> Vec<(i64, String, String, Option<String>, Option<i64>)> {
    let text = if table == "entries" {
        "body"
    } else {
        "summary"
    };
    let sql = format!(
        "SELECT {key},kind,{text},authority_basis,authority_entry FROM {table} ORDER BY {key}"
    );
    connection
        .prepare(&sql)
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}
