use super::*;
use crate::board::board_actor::{BoardActor, HarnessLabel};
use crate::board::board_domain::board_collections::{EntryCursor, OverviewReply};
use crate::board::board_protocol::BoardErrorCode;
use crate::board::board_protocol::ReadScope;
use crate::board::board_vocabulary::EntryKind;
use crate::board::local_board::{LocalBoard, board_queries::collection_reads};
use std::time::Duration;

fn database() -> (tempfile::TempDir, LocalBoard) {
    let directory = crate::board::board_test_support::scratch("trufflepig-nested-");
    let board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(20),
    )
    .unwrap();
    board.conn.execute_batch(
        "INSERT INTO actors VALUES(1,'josh','laptop','codex','s1'),(2,'josh','desktop','codex','s1');
         INSERT INTO plans(id,title,owner_user,head_revision,created_at)
 VALUES(1,'One','josh',1,1),(2,'Two','josh',1,1);
         INSERT INTO texts VALUES('one','text');"
    ).unwrap();
    entry(&board.conn, 1, 1, "create");
    entry(&board.conn, 2, 2, "create");
    board
        .conn
        .execute_batch(
            "INSERT INTO revisions VALUES(1,1,'one','create',1,1,1),(2,1,'one','create',2,1,2);",
        )
        .unwrap();
    (directory, board)
}

fn context() -> WriteContext {
    WriteContext {
        actor_id: -1,
        actor: BoardActor::new(
            "josh",
            "laptop",
            HarnessLabel::parse("codex").unwrap(),
            "s1",
        )
        .unwrap(),
        model: None,
        effort: None,
        now: 100,
        seq: EventSeq::new(0),
        claim_ttl_secs: 20,
        via: None,
    }
}

fn plan(number: u64) -> PlanId {
    PlanId::new(number).unwrap()
}
fn id(number: u64) -> EntryId {
    EntryId::new(number).unwrap()
}

fn entry(conn: &Connection, id: u64, seq: u64, kind: &str) {
    let id = i64::try_from(id).unwrap();
    let seq = i64::try_from(seq).unwrap();
    conn.execute(
        "INSERT INTO entries(id,plan_id,kind,body,actor_id,seq,created_at) VALUES(?1,1,?2,'summary',1,?3,50)",
        params![id, kind, seq],
    )
    .unwrap();
    conn.execute(
        concat!(
            "INSERT OR IGNORE INTO events(seq,plan_id,kind,subject,actor_id,summary,created_at) ",
            "VALUES(?1,1,?2,?3,1,'summary',50)"
        ),
        params![seq, kind, format!("E{id}")],
    )
    .unwrap();
}

fn task(conn: &Connection, ordinal: u64, seq: u64) {
    let ordinal = i64::try_from(ordinal).unwrap();
    let seq = i64::try_from(seq).unwrap();
    conn.execute(
        "INSERT INTO tasks VALUES(1,?1,'Task','todo',NULL,NULL,?2)",
        params![ordinal, seq],
    )
    .unwrap();
}

fn claim(conn: &Connection, number: u64, ordinal: u64, actor: i64, entry: u64, last_active: i64) {
    let number = i64::try_from(number).unwrap();
    let ordinal = i64::try_from(ordinal).unwrap();
    let entry = i64::try_from(entry).unwrap();
    conn.execute(
        concat!(
            "INSERT INTO claims(id,plan_id,task_ordinal,actor_id,entry_id,scope,claimed_at,last_active) ",
            "VALUES(?1,1,?2,?3,?4,'scope',?1,?5)"
        ),
        params![number, ordinal, actor, entry, last_active],
    )
    .unwrap();
}

fn tasks(
    conn: &Connection,
    after: Option<TaskId>,
    ceiling: Option<TaskCeiling>,
    through: Option<EventSeq>,
    limit: usize,
) -> TaskPage {
    match tasks_page(
        conn,
        plan(1),
        after,
        ceiling,
        through,
        limit,
        TaskSelection::default(),
    )
    .unwrap()
    .result
    {
        BoardResult::Tasks(page) => page,
        other => panic!("unexpected {other:?}"),
    }
}

fn overview(conn: &Connection, after: Option<PlanId>, through: Option<EventSeq>) -> OverviewReply {
    match collection_reads::overview(conn, &context(), &ReadScope::All, after, through, 1)
        .unwrap()
        .result
    {
        BoardResult::Overview(page) => page,
        other => panic!("unexpected {other:?}"),
    }
}

mod claim_cursor_tests;
mod done_paging_tests;
mod entry_backref_tests;
mod nested_scope_tests;
mod task_ceiling_tests;
