mod claim_activity_tests;
mod claim_assignment_tests;
mod claim_concurrency_tests;
mod claim_delegation_authority_tests;
mod claim_delegation_tests;
mod claim_receipt_tests;
mod claim_resume_entry_tests;
mod claim_resume_race_tests;
mod claim_vendor_tests;
mod completed_task_tests;
mod worker_lifecycle_tests;

mod claim_resumption;

use std::path::PathBuf;
use std::sync::{Arc, Barrier};

use super::*;
use crate::board_actor::BoardActor;
use crate::board_backend::BoardBackend;
use crate::board_protocol::{BoardOp, BoardRequest, BoardResult};
use crate::board_vocabulary::TaskColumn;
use crate::local_board::LocalBoard;
use tp_core::request_context::HarnessLabel;

struct ClaimDatabase {
    _directory: tempfile::TempDir,
    path: PathBuf,
}

impl ClaimDatabase {
    fn new() -> Self {
        let directory = tp_core::test_fixtures::scratch("task-claim-tests-");
        let path = directory.path().join("board.sqlite3");
        let database = Self {
            _directory: directory,
            path,
        };
        let conn = database.connect();
        conn.execute(
            concat!(
                "INSERT INTO plans(id,title,owner_user,head_revision,next_task,created_at) ",
                "VALUES(1,'Plan','josh',1,0,100)"
            ),
            [],
        )
        .unwrap();
        database
    }

    fn connect(&self) -> Connection {
        crate::local_board::board_database::open(&self.path)
            .unwrap()
            .0
    }
}

fn actor(user: &str, harness: &str, session: &str) -> BoardActor {
    BoardActor::new(
        user,
        "test-host",
        HarnessLabel::parse(harness).unwrap(),
        session,
    )
    .unwrap()
}

fn grant_legacy_claude(conn: &Connection) {
    conn.execute(
        "INSERT OR IGNORE INTO actors(user,host,harness,session) VALUES('system','board-v10','system','migration')",
        [],
    )
    .unwrap();
    let system_actor: i64 = conn
        .query_row(
            "SELECT id FROM actors WHERE user='system' AND host='board-v10' AND harness='system' AND session='migration'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let seq: i64 = conn
        .query_row(
            "SELECT max(max_entry,max_event) FROM (SELECT COALESCE(MAX(seq),0) AS max_entry FROM entries), (SELECT COALESCE(MAX(seq),0) AS max_event FROM events)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let entry: i64 = conn
        .query_row(
            "INSERT INTO entries(plan_id,kind,body,to_whom,actor_id,seq,created_at,authority_basis) VALUES(1,'grant','migration fixture: all roles to claude','claude',?1,?2,100,'owner') RETURNING id",
            rusqlite::params![system_actor, seq + 1],
            |row| row.get(0),
        )
        .unwrap();
    conn.execute(
        "INSERT INTO events(seq,plan_id,kind,subject,to_whom,actor_id,summary,created_at,authority_basis) VALUES(?1,1,'grant',?2,'claude',?3,'migration fixture: all roles to claude',100,'owner')",
        rusqlite::params![seq + 1, format!("E{entry}"), system_actor],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO authority_grants(entry_id,plan_id,owner_user,roles_json,grantee,granted_at,expires_at,revoked_by_entry,source) VALUES(?1,1,'josh','[\"triage\",\"release\",\"link\",\"assign\",\"decide\"]','claude',100,NULL,NULL,'migration')",
        [entry],
    )
    .unwrap();
}

fn write(
    conn: &mut Connection,
    actor: &BoardActor,
    now: i64,
    operation: impl FnOnce(&Transaction<'_>, &WriteContext) -> Result<BoardReply, BoardError>,
) -> Result<BoardReply, BoardError> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let actor_id = crate::local_board::ensure_actor(&tx, actor, now).unwrap();
    let ctx = WriteContext {
        actor_id,
        actor: actor.clone(),
        model: Some("test-model".into()),
        effort: Some("xhigh".into()),
        now,
        seq: EventSeq::new(crate::local_board::max_seq(&tx).unwrap().get() + 1),
        claim_ttl_secs: 120,
        via: None,
        origin: None,
        authority: None,
    };
    let reply = operation(&tx, &ctx)?;
    tx.commit().unwrap();
    Ok(reply)
}

fn carved_task(conn: &mut Connection, actor: &BoardActor, now: i64, scope: &str) -> TaskId {
    let reply = write(conn, actor, now, |tx, ctx| {
        carve_claim(
            tx,
            ctx,
            PlanId::new(1).unwrap(),
            &PlanTitle::new("Claim lane").unwrap(),
            &EntryText::new(scope).unwrap(),
            Some("Claims"),
        )
    })
    .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("expected mutation reply");
    };
    change.task.unwrap()
}

fn claim(
    conn: &mut Connection,
    actor: &BoardActor,
    now: i64,
    task: TaskId,
    scope: &str,
) -> Result<BoardReply, BoardError> {
    write(conn, actor, now, |tx, ctx| {
        claim_task(
            tx,
            ctx,
            task,
            Some(&EntryText::new(scope).unwrap()),
            ClaimResume::No,
            None,
        )
    })
}

#[test]
fn second_connection_conflict_names_snapshot_and_activity() {
    let database = ClaimDatabase::new();
    let mut first = database.connect();
    let mut second = database.connect();
    let holder = actor("josh", "codex", "one");
    let task = carved_task(&mut first, &holder, 1000, "parser and tests");
    first
        .execute(
            "UPDATE agent_sessions SET model='replacement',effort='low'",
            [],
        )
        .unwrap();
    let error = claim(
        &mut second,
        &actor("josh", "muse", "two"),
        1100,
        task,
        "different lane",
    )
    .unwrap_err()
    .to_string();
    assert!(error.starts_with("claim_conflict:"), "{error}");
    assert!(error.contains(&holder.identity()));
    assert!(error.contains("test-model/xhigh"));
    assert!(error.contains("since 1000, active 100s ago (last activity 1000)"));
    assert_eq!(
        read_claims_window(&first, task.plan, i64::MIN, i64::MAX, 1100, 120)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn stale_takeover_preserves_scope_and_targets_previous_holder() {
    let database = ClaimDatabase::new();
    let mut first = database.connect();
    let mut second = database.connect();
    let old = actor("josh", "codex", "one");
    let new = actor("josh", "muse", "two");
    let task = carved_task(&mut first, &old, 1000, "old scope");
    assert!(
        !read_claims_window(&first, task.plan, i64::MIN, i64::MAX, 1120, 120).unwrap()[0].stale
    );
    assert!(claim(&mut second, &new, 1120, task, "new scope").is_err());
    assert!(read_claims_window(&first, task.plan, i64::MIN, i64::MAX, 1121, 120).unwrap()[0].stale);
    claim(&mut second, &new, 1121, task, "new scope").unwrap();
    let claims = read_claims_window(&first, task.plan, i64::MIN, i64::MAX, 1121, 120).unwrap();
    assert_eq!(claims.len(), 2);
    assert_eq!(claims[0].scope.as_str(), "old scope");
    assert_eq!(claims[0].ended_at, Some(1121));
    assert_eq!(claims[0].end_reason, Some(ClaimEndReason::TakenOver));
    assert_eq!(claims[1].actor, new);
    assert_eq!(claims[1].scope.as_str(), "new scope");
    let recipient: String = first
        .query_row(
            "SELECT to_whom FROM events ORDER BY seq DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(recipient, old.identity());
    let summary: String = first
        .query_row(
            "SELECT summary FROM events ORDER BY seq DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(summary.contains("took over stale claim from"));
}
