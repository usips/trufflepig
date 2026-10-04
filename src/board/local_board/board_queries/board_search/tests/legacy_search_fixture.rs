use super::*;
use std::path::PathBuf;

pub(super) fn counts(conn: &Connection) -> Vec<i64> {
    [
        "actors",
        "agent_sessions",
        "plans",
        "texts",
        "entries",
        "revisions",
        "proposals",
        "tasks",
        "claims",
        "events",
        "operation_dedupes",
        "board_feedback",
        "repos",
        "repo_paths",
        "plan_repos",
        "entry_refs",
        "commits",
        "commit_plans",
        "feedback_imports",
    ]
    .map(|table| {
        conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
    })
    .to_vec()
}

pub(super) fn populated_v1(directory: &tempfile::TempDir) -> (PathBuf, Vec<i64>) {
    let path = directory.path().join("legacy.sqlite3");
    let conn = Connection::open(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    conn.execute_batch(crate::board::local_board::board_database::legacy_schema())
        .unwrap();
    conn.pragma_update(None, "user_version", 1).unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    let revision_body = "migrationneedle repeated revision body";
    let proposal_body = "proposalneedle full proposal body";
    let revision_hash = blake3::hash(revision_body.as_bytes()).to_hex().to_string();
    let proposal_hash = blake3::hash(proposal_body.as_bytes()).to_hex().to_string();
    conn.execute(
        "INSERT INTO texts(hash,body) VALUES(?1,?2),(?3,?4)",
        params![revision_hash, revision_body, proposal_hash, proposal_body],
    )
    .unwrap();
    conn.execute_batch(r#"
INSERT INTO actors(id,user,host,harness,session) VALUES(1,'josh','host','human','search-test');
INSERT INTO agent_sessions(actor_id,cursor_seq,first_seen,last_seen) VALUES(1,2,1,5);
INSERT INTO plans(id,title,owner_user,head_revision,next_task,created_at)
 VALUES(1,'Legacy One','josh',1,1,1),(2,'Legacy Two','josh',1,0,2);
INSERT INTO entries(id,plan_id,kind,body,actor_id,state,seq,created_at) VALUES
 (1,1,'create','Legacy One',1,NULL,1,1),
 (2,2,'create','Legacy Two',1,NULL,2,2),
 (3,1,'claim','retained labor',1,NULL,3,3),
 (4,1,'proposal','summaryneedle proposal',1,'open',4,4),
 (5,NULL,'feedback','globalneedle feedback',1,'open',5,5);
INSERT INTO tasks(plan_id,ordinal,title,column_name,seq) VALUES(1,1,'Retained task','doing',3);
INSERT INTO claims(id,plan_id,task_ordinal,actor_id,entry_id,scope,claimed_at,last_active)
 VALUES(1,1,1,1,3,'retained scope',3,5);
INSERT INTO events(seq,plan_id,kind,subject,actor_id,summary,created_at) VALUES
 (1,1,'create','E1',1,'Legacy One',1),
 (2,2,'create','E2',1,'Legacy Two',2),
 (3,1,'claim','E3',1,'retained labor',3),
 (4,1,'proposal','E4',1,'summaryneedle proposal',4),
 (5,NULL,'feedback','E5',1,'globalneedle feedback',5);
INSERT INTO board_feedback(entry_id,feedback_kind,version,cwd,recent_calls_json)
 VALUES(5,'missing','legacy','/legacy','[]');
INSERT INTO operation_dedupes(dedupe_key,reply_json,created_at) VALUES('legacy','{}',5);
INSERT INTO repos(repo_key,origin_label) VALUES('repo','legacy origin');
INSERT INTO repo_paths(repo_key,host,common_dir) VALUES('repo','host','/legacy/repo');
INSERT INTO plan_repos(plan_id,repo_key) VALUES(1,'repo');
INSERT INTO entry_refs(entry_id,target) VALUES(4,'P1@1');
INSERT INTO commits VALUES('repo','0123456789012345678901234567890123456789','retained commit',5,'author',
 '[]',1,'[]',2,1);
INSERT INTO commit_plans VALUES('repo','0123456789012345678901234567890123456789',1,1,1);
INSERT INTO feedback_imports(import_key,entry_id) VALUES('retained import',5);
"#).unwrap();
    conn.execute(
        concat!(
            "INSERT INTO revisions(plan_id,number,text_hash,source,entry_id,actor_id,seq) ",
            "VALUES(1,1,?1,'create',1,1,1),(2,1,?1,'create',2,1,2)"
        ),
        [&revision_hash],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO proposals(entry_id,plan_id,base_revision,text_hash,state) VALUES(4,1,1,?1,'open')",
        [&proposal_hash],
    )
    .unwrap();
    let before = counts(&conn);
    (path, before)
}
