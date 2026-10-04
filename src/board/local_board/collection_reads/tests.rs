use super::*;
use crate::board::board_actor::BoardActor;
use crate::board::board_protocol::{BoardErrorCode, EntryState};
use crate::board::board_vocabulary::ProposalState;
use crate::board::local_board::LocalBoard;
use std::time::Duration;

pub(super) fn database() -> (tempfile::TempDir, LocalBoard) {
    let directory = crate::board::board_test_support::scratch("trufflepig-collections-");
    let board = LocalBoard::open_path(
        &directory.path().join("board.sqlite3"),
        Duration::from_secs(20),
    )
    .unwrap();
    board.conn.execute_batch(
        "INSERT INTO actors VALUES(1,'josh','laptop','codex','s1'),(2,'josh','desktop','codex','s1'),(3,'josh','laptop','claude','s2'),(4,'other','laptop','codex','s1');
         INSERT INTO agent_sessions(actor_id,cursor_seq,first_seen,last_seen) VALUES(1,17,1,1);
         INSERT INTO plans(id,title,owner_user,steward,head_revision,created_at) VALUES(1,'One','josh','claude',1,1),(2,'Two','josh',NULL,1,1);
         INSERT INTO texts VALUES('one','full original plan text');"
    ).unwrap();
    seed_entry(&board.conn, 1, 1, 1, "create", 3, None, "One");
    seed_entry(&board.conn, 2, 2, 2, "create", 3, None, "Two");
    board
        .conn
        .execute_batch(
            "INSERT INTO revisions VALUES(1,1,'one','create',1,3,1),(2,1,'one','create',2,3,2);",
        )
        .unwrap();
    (directory, board)
}

pub(super) fn context() -> WriteContext {
    WriteContext {
        actor_id: 1,
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

pub(super) fn plan(number: u64) -> PlanId {
    PlanId::new(number).unwrap()
}
pub(super) fn id(number: u64) -> EntryId {
    EntryId::new(number).unwrap()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn seed_entry(
    conn: &Connection,
    entry: u64,
    seq: u64,
    plan: u64,
    kind: &str,
    actor: i64,
    to: Option<&str>,
    body: &str,
) {
    let entry = i64::try_from(entry).unwrap();
    let seq = i64::try_from(seq).unwrap();
    let plan = i64::try_from(plan).unwrap();
    conn.execute(
        "INSERT INTO entries(id,plan_id,kind,body,to_whom,actor_id,model,effort,seq,created_at) VALUES(?1,?2,?3,?4,?5,?6,'model','xhigh',?7,50)",
        params![entry, plan, kind, body, to, actor, seq],
    ).unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO events(seq,plan_id,kind,subject,to_whom,actor_id,model,effort,summary,created_at) VALUES(?1,?2,?3,?4,?5,?6,'model','xhigh',?7,50)",
        params![seq, plan, kind, format!("E{entry}"), to, actor, body],
    ).unwrap();
}

fn entries(
    conn: &Connection,
    after: Option<EntryCursor>,
    through: Option<EventSeq>,
    limit: usize,
) -> EntriesPage {
    match entries_page(
        conn,
        Some(plan(1)),
        None,
        None,
        None,
        None,
        None,
        None,
        after,
        through,
        limit,
    )
    .unwrap()
    .result
    {
        BoardResult::Entries(page) => page,
        other => panic!("unexpected {other:?}"),
    }
}

pub(super) fn register_repositories(conn: &Connection) -> RepoKey {
    let first = "a".repeat(40);
    let second = "b".repeat(40);
    conn.execute(
        "INSERT INTO repos VALUES(?1,NULL),(?2,NULL)",
        params![first, second],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO plan_repos VALUES(1,?1),(2,?2)",
        params![first, second],
    )
    .unwrap();
    RepoKey::parse(&first).unwrap()
}

fn seed_task(conn: &Connection, ordinal: u64) {
    let ordinal = i64::try_from(ordinal).unwrap();
    conn.execute(
        "INSERT INTO tasks VALUES(1,?1,?2,'todo',NULL,NULL,1)",
        params![ordinal, format!("Task {ordinal}")],
    )
    .unwrap();
}

fn seed_claim(conn: &Connection, ordinal: u64, actor: i64, last_active: i64) {
    let ordinal = i64::try_from(ordinal).unwrap();
    conn.execute(
        "INSERT INTO claims(plan_id,task_ordinal,actor_id,entry_id,scope,claimed_at,last_active) VALUES(1,?1,?2,1,'scope',10,?3)",
        params![ordinal, actor, last_active],
    ).unwrap();
}

fn seed_feedback(conn: &Connection, entry: u64, seq: u64, state: &str) {
    seed_entry(conn, entry, seq, 1, "feedback", 1, None, "feedback summary");
    let entry = i64::try_from(entry).unwrap();
    conn.execute(
        "UPDATE entries SET state=?1 WHERE id=?2",
        params![state, entry],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO board_feedback(entry_id,feedback_kind,version,build_id,cwd,steer_mode,recent_calls_json) VALUES(?1,'missing','v1','build1','src','plan',?2)",
        params![entry, r#"[{"verb":"search","args":["needle"],"exit_code":0,"error_prefix":null,"truncated":false,"coverage":"complete"}]"#],
    ).unwrap();
}

fn feedback(
    conn: &Connection,
    open_only: bool,
    after: Option<EntryCursor>,
    through: Option<EventSeq>,
    limit: usize,
) -> FeedbackPage {
    match feedback_page(conn, open_only, after, through, limit)
        .unwrap()
        .result
    {
        BoardResult::Feedback(page) => page,
        other => panic!("unexpected {other:?}"),
    }
}

mod attention_index_tests;
mod attention_proposal_tests;
mod collection_feedback_authority_tests;
mod entry_page_tests;
mod feedback_page_tests;
mod overview_attention_tests;
mod revision_page_tests;

fn authority_actor(user: &str, host: &str, harness: &str, session: &str) -> WriteContext {
    let mut ctx = context();
    ctx.actor =
        BoardActor::new(user, host, HarnessLabel::parse(harness).unwrap(), session).unwrap();
    ctx.actor_id = -1;
    ctx
}

fn attention_page(
    conn: &Connection,
    ctx: &WriteContext,
    repo: Option<&RepoKey>,
    all: bool,
    limit: usize,
) -> AttentionReply {
    match attention(conn, ctx, repo, all, None, None, limit)
        .unwrap()
        .result
    {
        BoardResult::Attention(page) => page,
        other => panic!("unexpected {other:?}"),
    }
}

fn attention_ids(page: &AttentionReply) -> Vec<EntryId> {
    page.entries.iter().map(|entry| entry.id).collect()
}
