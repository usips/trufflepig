//! SQLite-backed board state, serialized mutations, and immutable evidence.

mod board_database;
mod board_evidence;
mod board_feed;
mod board_lifecycle;
mod board_queries;
mod board_writes;
mod mutation_dispatch;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::board_actor::{BoardActor, BoardRecipient, HarnessLabel};
use super::board_backend::BoardBackend;
use super::board_config::BoardConfig;
use super::board_ids::{
    BoardRef, EntryId, EventSeq, PlanId, PlanRevision, RepoKey, TaskId, extract_refs,
};
use super::board_protocol::*;
use super::board_vocabulary::EntryKind;
pub use board_database::SCHEMA_VERSION;
#[cfg(test)]
pub(crate) use board_database::seed_storage_schema;
pub(super) use board_evidence::{can_accept, insert_entry, insert_event, require_plan};
use board_writes::board_receipts::{is_dedupable, receipt_current, request_dedupe_key};

pub struct LocalBoard {
    conn: Connection,
    reader: Option<Connection>,
    path: PathBuf,
    claim_ttl_secs: i64,
    #[cfg(test)]
    panic_after_write: bool,
}

impl BoardBackend for LocalBoard {
    fn handle(&mut self, request: &BoardRequest) -> Result<BoardReply, BoardError> {
        self.dispatch(request, false)
    }
    fn import_feedback(&mut self, request: &BoardRequest) -> Result<BoardReply, BoardError> {
        super::board_backend::ensure_feedback_import(request)?;
        self.dispatch(request, true)
    }
    fn max_seq(&self) -> Result<EventSeq, BoardError> {
        max_seq(self.reader.as_ref().unwrap_or(&self.conn))
    }
}

pub(super) struct WriteContext {
    pub actor_id: i64,
    pub actor: BoardActor,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub now: i64,
    pub seq: EventSeq,
    pub claim_ttl_secs: i64,
    pub via: Option<FeedbackVia>,
}

impl WriteContext {
    fn change_reply(
        &self,
        entry: EntryId,
        plan: Option<PlanId>,
        revision: Option<PlanRevision>,
        task: Option<TaskId>,
    ) -> BoardReply {
        self.reply_at(self.seq, entry, plan, revision, task)
    }

    /// A receipt for an in-place refresh points at an existing event
    /// sequence rather than the unwritten next one.
    fn change_reply_at(
        &self,
        seq: EventSeq,
        entry: EntryId,
        plan: Option<PlanId>,
        task: Option<TaskId>,
    ) -> BoardReply {
        self.reply_at(seq, entry, plan, None, task)
    }

    fn reply_at(
        &self,
        seq: EventSeq,
        entry: EntryId,
        plan: Option<PlanId>,
        revision: Option<PlanRevision>,
        task: Option<TaskId>,
    ) -> BoardReply {
        BoardReply::new(
            "local",
            BoardResult::Change(BoardChange {
                entry,
                seq,
                plan,
                revision,
                task,
                deduplicated: false,
            }),
        )
    }
}

pub(super) struct EntryDraft {
    pub plan_id: Option<PlanId>,
    pub kind: EntryKind,
    pub body: String,
    pub to_whom: Option<BoardRecipient>,
    pub supersedes: Option<EntryId>,
    pub repo_key: Option<RepoKey>,
    pub state: Option<String>,
}

pub(super) fn actor_from_row(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<BoardActor> {
    let harness: String = row.get(offset + 2)?;
    let harness = HarnessLabel::parse(&harness).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(offset + 2, rusqlite::types::Type::Text, e.into())
    })?;
    Ok(BoardActor {
        user: row.get(offset)?,
        host: row.get(offset + 1)?,
        harness,
        session: row.get(offset + 3)?,
    })
}

pub(super) fn delegated_actor_from_row(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<Option<BoardActor>> {
    let user: Option<String> = row.get(offset)?;
    let host: Option<String> = row.get(offset + 1)?;
    let harness: Option<String> = row.get(offset + 2)?;
    let session: Option<String> = row.get(offset + 3)?;
    match (user, host, harness, session) {
        (Some(user), Some(host), Some(harness), Some(session)) => {
            let harness = HarnessLabel::parse(&harness).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    offset + 2,
                    rusqlite::types::Type::Text,
                    e.into(),
                )
            })?;
            Ok(Some(BoardActor {
                user,
                host,
                harness,
                session,
            }))
        }
        (None, None, None, None) => Ok(None),
        _ => Err(rusqlite::Error::FromSqlConversionFailure(
            offset,
            rusqlite::types::Type::Null,
            "claim delegation references a missing actor".into(),
        )),
    }
}

pub(super) fn read_entry(conn: &Connection, id: EntryId) -> Result<EntryRecord, BoardError> {
    board_queries::board_reads::entry(conn, id)
}

pub(super) fn sql_error(error: rusqlite::Error) -> BoardError {
    BoardError::from(anyhow::anyhow!(error))
}
pub(super) fn invalid(code: &str, message: impl Into<String>) -> BoardError {
    BoardError::from(anyhow::anyhow!("{code}: {}", message.into()))
}
pub(super) fn max_seq(conn: &Connection) -> Result<EventSeq, BoardError> {
    let seq: u64 = conn
        .query_row("SELECT COALESCE(MAX(seq),0) FROM events", [], |r| {
            row_number(r, 0)
        })
        .map_err(sql_error)?;
    Ok(EventSeq::new(seq))
}
pub(super) fn unix_now() -> Result<i64, BoardError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| invalid("board_unavailable", e.to_string()))?
        .as_secs();
    i64::try_from(seconds).map_err(|e| invalid("board_unavailable", e.to_string()))
}

fn lookup_actor(conn: &Connection, actor: &BoardActor) -> Result<Option<i64>, BoardError> {
    conn.query_row(
        "SELECT id FROM actors WHERE user=?1 AND host=?2 AND harness=?3 AND session=?4",
        params![
            actor.user,
            actor.host,
            actor.harness.as_str(),
            actor.session
        ],
        |row| row.get(0),
    )
    .optional()
    .map_err(sql_error)
}

fn insert_actor(tx: &Transaction<'_>, actor: &BoardActor) -> Result<i64, BoardError> {
    tx.execute(
        "INSERT OR IGNORE INTO actors(user,host,harness,session) VALUES(?1,?2,?3,?4)",
        params![
            actor.user,
            actor.host,
            actor.harness.as_str(),
            actor.session
        ],
    )
    .map_err(sql_error)?;
    tx.query_row(
        "SELECT id FROM actors WHERE user=?1 AND host=?2 AND harness=?3 AND session=?4",
        params![
            actor.user,
            actor.host,
            actor.harness.as_str(),
            actor.session
        ],
        |r| r.get(0),
    )
    .map_err(sql_error)
}

fn ensure_actor(tx: &Transaction<'_>, actor: &BoardActor, now: i64) -> Result<i64, BoardError> {
    let id = insert_actor(tx, actor)?;
    tx.execute(
        concat!(
            "INSERT INTO agent_sessions(actor_id,first_seen,last_seen) VALUES(?1,?2,?2) ",
            "ON CONFLICT(actor_id) DO UPDATE SET last_seen=excluded.last_seen"
        ),
        params![id, now],
    )
    .map_err(sql_error)?;
    Ok(id)
}

/// Ensures delegate rows without marking activity.
/// Delegation resolves a holder that may never have acted: a new session
/// row starts with `last_seen` 0 (never seen) and an existing row keeps
/// its stored value untouched.
fn ensure_actor_without_seen_bump(
    tx: &Transaction<'_>,
    actor: &BoardActor,
    now: i64,
) -> Result<i64, BoardError> {
    let id = insert_actor(tx, actor)?;
    tx.execute(
        "INSERT OR IGNORE INTO agent_sessions(actor_id,first_seen,last_seen) VALUES(?1,?2,0)",
        params![id, now],
    )
    .map_err(sql_error)?;
    Ok(id)
}

pub(super) fn sqlite_id(value: u64) -> Result<i64, BoardError> {
    i64::try_from(value)
        .map_err(|_| invalid("invalid_reference", "number exceeds SQLite integer range"))
}

pub(super) fn sqlite_u64(value: i64) -> Result<u64, BoardError> {
    u64::try_from(value).map_err(|_| invalid("board_unavailable", "negative stored number"))
}

pub(super) struct SqlU64(u64);

pub(super) fn sql_number(value: u64) -> SqlU64 {
    SqlU64(value)
}

impl rusqlite::ToSql for SqlU64 {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        let number = i64::try_from(self.0)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        Ok(rusqlite::types::ToSqlOutput::Owned(number.into()))
    }
}

pub(super) fn row_number(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(e),
        )
    })
}
