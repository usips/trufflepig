use super::*;
use crate::board::board_ids::{EventSeq, PlanId};
use crate::board::board_protocol::BoardResult;
use crate::board::board_vocabulary::EntryText;
use crate::{board::board_actor::BoardActor, request_context::HarnessLabel};
use rusqlite::{Connection, Transaction};

struct LifecycleDatabase {
    _directory: tempfile::TempDir,
    conn: Connection,
}

impl LifecycleDatabase {
    fn new(task_column: &str) -> Self {
        let directory = crate::test_fixtures::scratch("plan-lifecycle-tests-");
        let path = directory.path().join("board.sqlite3");
        let conn = crate::board::local_board::board_database::open(&path)
            .unwrap()
            .0;
        conn.execute(
            "INSERT INTO actors(id,user,host,harness,session) VALUES(1,'josh','test-host','human','owner')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO plans(id,title,owner_user,head_revision,next_task,created_at) VALUES(1,'Lifecycle','josh',1,1,1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks(plan_id,ordinal,title,column_name,seq) VALUES(1,1,'Task',?1,1)",
            [task_column],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO entries(id,plan_id,kind,body,actor_id,seq,created_at) VALUES(1,1,'plan','initial plan',1,1,1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events(seq,plan_id,kind,subject,actor_id,summary,created_at) VALUES(1,1,'plan','E1',1,'initial plan',1)",
            [],
        )
        .unwrap();
        Self {
            _directory: directory,
            conn,
        }
    }
}

fn owner() -> BoardActor {
    BoardActor::new(
        "josh",
        "test-host",
        HarnessLabel::parse("human").unwrap(),
        "owner",
    )
    .unwrap()
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
    let actor_id = crate::board::local_board::ensure_actor(&tx, actor, now).unwrap();
    let ctx = WriteContext {
        actor_id,
        actor: actor.clone(),
        model: Some("test-model".into()),
        effort: Some("xhigh".into()),
        now,
        seq: EventSeq::new(crate::board::local_board::max_seq(&tx).unwrap().get() + 1),
        claim_ttl_secs: 120,
        via: None,
        origin: None,
        authority: None,
    };
    let reply = operation(&tx, &ctx)?;
    tx.commit().unwrap();
    Ok(reply)
}

fn close(conn: &mut Connection, now: i64) -> Result<BoardReply, BoardError> {
    write(conn, &owner(), now, |tx, ctx| {
        close_plan(
            tx,
            ctx,
            PlanId::new(1).unwrap(),
            &EntryText::new("Implementation is complete").unwrap(),
        )
    })
}

#[test]
fn close_requires_completed_or_transferred_tasks_and_has_no_partial_effect() {
    let mut database = LifecycleDatabase::new("todo");
    let error = close(&mut database.conn, 10).unwrap_err();
    assert!(error.to_string().starts_with("invalid_state:"), "{error}");
    let (state, entry_count): (String, i64) = database
        .conn
        .query_row(
            "SELECT lifecycle_state,(SELECT count(*) FROM entries WHERE kind='plan_close') FROM plans WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((state.as_str(), entry_count), ("open", 0));
}

#[test]
fn close_requires_no_live_claims_even_when_task_is_done() {
    let mut database = LifecycleDatabase::new("done");
    database
        .conn
        .execute(
            "INSERT INTO claims(id,plan_id,task_ordinal,actor_id,entry_id,scope,claimed_at,last_active) VALUES(1,1,1,1,1,'active work',2,2)",
            [],
        )
        .unwrap();
    let error = close(&mut database.conn, 10).unwrap_err();
    assert!(error.to_string().starts_with("invalid_state:"), "{error}");
    let state: String = database
        .conn
        .query_row("SELECT lifecycle_state FROM plans WHERE id=1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(state, "open");
}

#[test]
fn close_and_reopen_preserve_task_acceptance_and_open_obligations() {
    let mut database = LifecycleDatabase::new("done");
    database
        .conn
        .execute_batch(
            "INSERT INTO texts(hash,body) VALUES('plan-body','plan'),('proposal-body','proposal');
             INSERT INTO revisions(plan_id,number,text_hash,source,entry_id,actor_id,seq)
               VALUES(1,1,'plan-body','create',1,1,1);
             INSERT INTO entries(id,plan_id,kind,body,actor_id,seq,created_at)
               VALUES(2,1,'proposal','open proposal',1,2,2),
                     (3,1,'question','unanswered question',1,3,3);
             INSERT INTO proposals(entry_id,plan_id,base_revision,text_hash,state)
               VALUES(2,1,1,'proposal-body','open');
             INSERT INTO events(seq,plan_id,kind,subject,actor_id,summary,created_at)
               VALUES(2,1,'proposal','E2',1,'open proposal',2),
                     (3,1,'question','E3',1,'unanswered question',3);",
        )
        .unwrap();

    let closed = close(&mut database.conn, 10).unwrap();
    let BoardResult::Change(change) = closed.result else {
        panic!("expected lifecycle change");
    };
    assert_eq!(change.plan, Some(PlanId::new(1).unwrap()));
    let task_column: String = database
        .conn
        .query_row(
            "SELECT column_name FROM tasks WHERE plan_id=1 AND ordinal=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let (plan_state, proposal_state, unresolved_question): (String, String, bool) = database
        .conn
        .query_row(
            concat!(
                "SELECT p.lifecycle_state,proposal.state,NOT EXISTS(SELECT 1 FROM entries answer ",
                "JOIN entry_refs reference ON reference.entry_id=answer.id ",
                "WHERE answer.kind='answer' AND reference.target='E3') FROM plans p ",
                "JOIN proposals proposal ON proposal.entry_id=2 WHERE p.id=1"
            ),
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(task_column, "done");
    assert_eq!(
        (
            plan_state.as_str(),
            proposal_state.as_str(),
            unresolved_question
        ),
        ("closed", "open", true)
    );

    write(&mut database.conn, &owner(), 11, |tx, ctx| {
        reopen_plan(
            tx,
            ctx,
            PlanId::new(1).unwrap(),
            &EntryText::new("Resume follow-up work").unwrap(),
        )
    })
    .unwrap();
    let (plan_state, task_column, proposal_state): (String, String, String) = database
        .conn
        .query_row(
            concat!(
                "SELECT p.lifecycle_state,task.column_name,proposal.state FROM plans p ",
                "JOIN tasks task ON task.plan_id=p.id JOIN proposals proposal ON proposal.entry_id=2 ",
                "WHERE p.id=1 AND task.ordinal=1"
            ),
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (
            plan_state.as_str(),
            task_column.as_str(),
            proposal_state.as_str()
        ),
        ("open", "done", "open")
    );
}
