use super::*;

#[test]
fn populated_v1_duplicate_checkout_paths_keep_first_key_and_all_evidence() {
    use crate::board::board_ids::RepoKey;
    use crate::identity::GitOid;
    let directory = directory();
    let path = directory.path().join("board.sqlite3");
    let legacy = Connection::open(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let first =
        RepoKey::from_roots([GitOid::parse("1111111111111111111111111111111111111111").unwrap()])
            .unwrap();
    let duplicate =
        RepoKey::from_roots([GitOid::parse("2222222222222222222222222222222222222222").unwrap()])
            .unwrap();
    let (first, duplicate) = if first.as_str() > duplicate.as_str() {
        (first, duplicate)
    } else {
        (duplicate, first)
    };
    legacy.execute_batch(SCHEMA_V1).unwrap();
    let fixture = r#"
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

    INSERT INTO repos VALUES('duplicate','second origin');
    INSERT INTO repo_paths VALUES('duplicate','host','/repo','duplicate scan','duplicate digest');
    INSERT INTO plans VALUES(2,'Second plan','josh','codex',1,1,11);
    INSERT INTO plan_repos VALUES(2,'duplicate');
    INSERT INTO entries VALUES(4,2,'decision','second created',NULL,NULL,1,'model','xhigh','duplicate',NULL,'old',3,11);
    INSERT INTO entries VALUES(5,2,'commit','second evidence',NULL,NULL,1,'model','xhigh','duplicate',NULL,'old',4,12);
    INSERT INTO revisions VALUES(2,1,'body','create',4,1,3);
    INSERT INTO tasks VALUES(2,1,'Second task','doing','codex','Scope',3);
    INSERT INTO claims VALUES(2,2,1,1,5,'second scope',11,21,NULL,NULL);
    INSERT INTO commits VALUES('duplicate','0123456789012345678901234567890123456789','second commit',12,
 'second author','[]',0,'[]',0,0);
    INSERT INTO commit_plans VALUES('duplicate','0123456789012345678901234567890123456789',2,1,5);
    INSERT INTO events VALUES(3,2,'decision','P2',NULL,1,'second created',11);
    INSERT INTO events VALUES(4,2,'commit','E5',NULL,1,'second evidence',12);
    PRAGMA user_version=1;
    "#.replace("'repo'",&format!("'{}'",first.as_str())).replace("'duplicate'",&format!("'{}'",duplicate.as_str()));
    legacy.execute_batch(&fixture).unwrap();
    drop(legacy);
    let (conn, _) = open(&path).unwrap();
    assert_eq!(
        conn.query_row("SELECT repo_key FROM repo_paths", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        first.as_str()
    );
    for (table, count) in [
        ("repo_paths", 1),
        ("repos", 2),
        ("plans", 2),
        ("entries", 5),
        ("revisions", 2),
        ("claims", 2),
        ("commits", 2),
        ("commit_plans", 2),
        ("commit_tasks", 2),
        ("events", 4),
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
        conn.query_row(
            "SELECT count(*) FROM plan_repos WHERE plan_id=2",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    assert_eq!(
        conn.query_row("SELECT repo_key FROM entries WHERE id=5", [], |row| row
            .get::<_, String>(
            0
        ))
        .unwrap(),
        duplicate.as_str()
    );
    assert!(
        conn.execute(
            "INSERT INTO repo_paths(repo_key,host,common_dir) VALUES(?1,'host','/repo')",
            [duplicate.as_str()]
        )
        .is_err()
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
    let crate::board::board_protocol::BoardResult::Repositories(targets) =
        crate::board::local_board::board_queries::board_reads::repositories(&conn, None)
            .unwrap()
            .result
    else {
        panic!("missing targets");
    };
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].registration.repo_key, first);
    assert_eq!(targets[0].plans.len(), 2);
}
