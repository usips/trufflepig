use super::*;
use crate::board::board_actor::BoardActor;
use crate::board::board_ids::EventSeq;
use crate::board::board_protocol::{BoardResult, ReadScope, TaskOrder};
use crate::board::board_vocabulary::{EntryText, TaskColumn};
use crate::board::local_board::board_queries::board_reads::{open_entries, task_view};
use crate::board::local_board::board_queries::collection_nested::{
    TaskSelection, overview_task_window, tasks_page,
};
use rusqlite::{Connection, Transaction};
use tp_core::request_context::HarnessLabel;

struct TransferDatabase {
    _directory: tempfile::TempDir,
    conn: Connection,
}

impl TransferDatabase {
    fn new() -> Self {
        let directory = tp_core::test_fixtures::scratch("task-transfer-tests-");
        let path = directory.path().join("board.sqlite3");
        let conn = crate::board::local_board::board_database::open(&path)
            .unwrap()
            .0;
        conn.execute_batch(
            "INSERT INTO actors(id,user,host,harness,session)
               VALUES(1,'josh','test-host','human','owner'),
                     (2,'other','test-host','muse','worker');
             INSERT INTO plans(id,title,owner_user,head_revision,next_task,created_at)
               VALUES(1,'Source','josh',1,1,1),(2,'Destination','josh',1,0,1);
             INSERT INTO tasks(plan_id,ordinal,title,column_name,assignee,section,seq)
               VALUES(1,1,'Preserve the trail','blocked','muse','Review',1);
             INSERT INTO entries(id,plan_id,kind,body,to_whom,actor_id,seq,created_at)
               VALUES(1,1,'task','created source task',NULL,1,1,1),
                     (2,1,'question','Which behavior should be preserved?','P1.1',1,2,2),
                     (3,1,'question','Plan-level question for this task','P1.1',1,3,3);
             INSERT INTO entry_refs(entry_id,target) VALUES(1,'P1.1'),(2,'P1.1');
             INSERT INTO events(seq,plan_id,kind,subject,actor_id,summary,created_at)
               VALUES(1,1,'task','P1.1',1,'created source task',1),
                     (2,1,'question','E2',1,'Which behavior should be preserved?',2),
                     (3,1,'question','E3',1,'Plan-level question for this task',3);",
        )
        .unwrap();
        Self {
            _directory: directory,
            conn,
        }
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

fn reader(actor: BoardActor, now: i64) -> WriteContext {
    WriteContext {
        actor_id: -1,
        actor,
        model: None,
        effort: None,
        now,
        seq: EventSeq::new(0),
        claim_ttl_secs: 120,
        via: None,
        origin: None,
        authority: None,
    }
}

#[test]
fn transfer_preserves_source_and_destination_provenance_and_routes_open_questions() {
    let mut database = TransferDatabase::new();
    let owner = actor("josh", "human", "owner");
    let reply = write(&mut database.conn, &owner, 10, |tx, ctx| {
        transfer_task(
            tx,
            ctx,
            TaskId::new(PlanId::new(1).unwrap(), 1).unwrap(),
            PlanId::new(2).unwrap(),
            &EntryText::new("Carry the decision trail").unwrap(),
        )
    })
    .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("expected transfer change");
    };
    let source = change.task.unwrap();
    let destination = change.transferred_to.unwrap();
    assert_eq!(source.to_string(), "P1.1");
    assert_eq!(destination.to_string(), "P2.1");
    let recipients: (Option<String>, Option<String>) = database.conn.query_row(
        "SELECT e.to_whom,event.to_whom FROM entries e JOIN events event ON event.seq=e.seq WHERE e.id=?1",
        [sql_number(change.entry.get())], |row| Ok((row.get(0)?,row.get(1)?))
    ).unwrap();
    assert_eq!(recipients, (Some("muse".into()), Some("muse".into())));

    let (source_column, source_seq): (String, i64) = database
        .conn
        .query_row(
            "SELECT column_name,seq FROM tasks WHERE plan_id=1 AND ordinal=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let destination_status: String = database
        .conn
        .query_row(
            "SELECT column_name FROM tasks WHERE plan_id=2 AND ordinal=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(source_column, "blocked");
    assert_eq!(source_seq, i64::try_from(change.seq.get()).unwrap());
    assert_eq!(destination_status, "blocked");
    let authorization: (String, Option<i64>, String, Option<i64>, Option<String>) = database
        .conn
        .query_row(
            concat!(
                "SELECT source_decide_basis,source_decide_entry,destination_decide_basis,",
                "destination_decide_entry,destination_assign_basis FROM task_transfers"
            ),
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        authorization,
        (
            "owner".into(),
            None,
            "owner".into(),
            None,
            Some("owner".into())
        )
    );
    assert!(database
        .conn
        .execute(
            "UPDATE task_transfers SET source_ordinal=source_ordinal WHERE source_plan=1 AND source_ordinal=1",
            [],
        )
        .is_err());
    assert!(
        database
            .conn
            .execute(
                "DELETE FROM task_transfers WHERE source_plan=1 AND source_ordinal=1",
                [],
            )
            .is_err()
    );
    let transfer_count: i64 = database
        .conn
        .query_row("SELECT count(*) FROM task_transfers", [], |row| row.get(0))
        .unwrap();
    assert_eq!(transfer_count, 1);
    let source_view = task_view(&database.conn, &reader(owner.clone(), 11), source).unwrap();
    assert_eq!(source_view.task.column, TaskColumn::Transferred);
    assert_eq!(source_view.task.transferred_to, Some(destination));
    assert_eq!(source_view.open_questions, 2);
    assert!(
        source_view
            .entries
            .iter()
            .any(|entry| entry.id == EntryId::new(2).unwrap())
    );
    assert!(
        source_view
            .entries
            .iter()
            .any(|entry| entry.id == EntryId::new(3).unwrap())
    );
    let destination_view =
        task_view(&database.conn, &reader(owner.clone(), 11), destination).unwrap();
    assert_eq!(destination_view.task.column, TaskColumn::Blocked);
    assert_eq!(destination_view.task.transferred_from, Some(source));
    write(&mut database.conn, &owner, 11, |tx, ctx| {
        super::super::plan_lifecycle::close_plan(
            tx,
            ctx,
            source.plan,
            &EntryText::new("The remaining question follows its task").unwrap(),
        )
    })
    .unwrap();
    assert_eq!(
        task_view(&database.conn, &reader(owner.clone(), 12), source)
            .unwrap()
            .open_questions,
        2
    );

    let transferred = tasks_page(
        &database.conn,
        source.plan,
        None,
        None,
        None,
        10,
        TaskSelection {
            column: Some(TaskColumn::Transferred),
            order: TaskOrder::Ordinal,
            before: None,
        },
    )
    .unwrap();
    let BoardResult::Tasks(transferred) = transferred.result else {
        panic!("expected task page");
    };
    assert_eq!(transferred.tasks.len(), 1);
    assert_eq!(transferred.tasks[0].id, source);
    let active = overview_task_window(&database.conn, source.plan, 10).unwrap();
    assert!(active.tasks.is_empty());
    assert_eq!(active.done_count, 0);

    database
        .conn
        .execute_batch(
            "INSERT INTO tasks(plan_id,ordinal,title,column_name,seq)
               VALUES(1,2,'Wrong kind endpoint','todo',1),(2,2,'Wrong source endpoint','todo',1);
             INSERT INTO entries(id,plan_id,kind,body,actor_id,seq,created_at)
               VALUES(40,2,'task_transfer','wrong source plan',1,3,3);",
        )
        .unwrap();
    let transfer_row = |entry: i64| {
        database.conn.execute(
            concat!(
                "INSERT INTO task_transfers(source_plan,source_ordinal,destination_plan,",
                "destination_ordinal,entry_id,source_decide_basis,destination_decide_basis) ",
                "VALUES(1,2,2,2,?1,'owner','owner')"
            ),
            [entry],
        )
    };
    assert!(transfer_row(1).is_err());
    assert!(transfer_row(40).is_err());

    database
        .conn
        .execute(
            "INSERT INTO entries(id,plan_id,kind,body,actor_id,seq,created_at) VALUES(50,2,'claim','successor claim',2,6,6)",
            [],
        )
        .unwrap();
    database
        .conn
        .execute(
            "INSERT INTO events(seq,plan_id,kind,subject,actor_id,summary,created_at) VALUES(6,2,'claim','P2.1',2,'successor claim',6)",
            [],
        )
        .unwrap();
    database
        .conn
        .execute(
            "INSERT INTO claims(id,plan_id,task_ordinal,actor_id,entry_id,scope,claimed_at,last_active) VALUES(1,2,1,2,50,'successor work',6,6)",
            [],
        )
        .unwrap();
    let recipient = reader(actor("other", "muse", "worker"), 7);
    let (reminders, _, _) = open_entries(
        &database.conn,
        &recipient,
        &ReadScope::All,
        20,
        crate::board::local_board::max_seq(&database.conn).unwrap(),
    )
    .unwrap();
    assert!(
        reminders
            .iter()
            .any(|entry| entry.id == EntryId::new(2).unwrap())
    );
    assert!(
        reminders
            .iter()
            .any(|entry| entry.id == EntryId::new(3).unwrap())
    );
    let plan_owner = reader(actor("josh", "human", "web"), 7);
    let (owner_reminders, _, _) = open_entries(
        &database.conn,
        &plan_owner,
        &ReadScope::All,
        20,
        crate::board::local_board::max_seq(&database.conn).unwrap(),
    )
    .unwrap();
    assert!(
        owner_reminders
            .iter()
            .any(|entry| entry.id == EntryId::new(3).unwrap())
    );
}

#[test]
fn transfer_refuses_a_live_claim_without_creating_a_successor() {
    let mut database = TransferDatabase::new();
    database
        .conn
        .execute(
            "INSERT INTO claims(id,plan_id,task_ordinal,actor_id,entry_id,scope,claimed_at,last_active) VALUES(1,1,1,1,1,'still working',2,2)",
            [],
        )
        .unwrap();
    let owner = actor("josh", "human", "owner");
    let error = write(&mut database.conn, &owner, 10, |tx, ctx| {
        transfer_task(
            tx,
            ctx,
            TaskId::new(PlanId::new(1).unwrap(), 1).unwrap(),
            PlanId::new(2).unwrap(),
            &EntryText::new("must wait for live work").unwrap(),
        )
    })
    .unwrap_err();
    assert!(error.to_string().starts_with("invalid_state:"), "{error}");
    let (task_count, transfer_count): (i64, i64) = database
        .conn
        .query_row(
            "SELECT (SELECT count(*) FROM tasks),(SELECT count(*) FROM task_transfers)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((task_count, transfer_count), (1, 0));
}
