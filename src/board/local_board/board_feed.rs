//! Fresh events and separate reminders; only explicit acknowledgements move cursors.

#[cfg(test)]
mod tests;

use rusqlite::{Connection, Transaction, params};

use super::{
    BoardError, WriteContext, actor_from_row, board_reads, invalid, max_seq, row_number, sql_error,
    sql_number, sqlite_u64, task_claims,
};
use crate::board::board_actor::BoardRecipient;
use crate::board::board_ids::{BoardRef, EventSeq, PlanId};
use crate::board::board_protocol::{BoardReply, BoardResult, EventRecord, InboxReply, InboxWait};
use crate::board::board_vocabulary::EntryText;

const FIRST_FEED_LIMIT: usize = 20;
const EVENT_SELECT: &str = "SELECT e.seq,e.plan_id,e.kind,e.subject,e.to_whom,a.user,a.host,a.harness,a.session,(SELECT model FROM entries WHERE seq=e.seq AND actor_id=e.actor_id ORDER BY id LIMIT 1),(SELECT effort FROM entries WHERE seq=e.seq AND actor_id=e.actor_id ORDER BY id LIMIT 1),e.summary,e.created_at FROM events e JOIN actors a ON a.id=e.actor_id";
const RELEVANT_EVENT: &str = "e.actor_id<>?1 AND (e.kind IN ('claim','task') OR e.to_whom IS NULL OR e.to_whom IN (?3,?4,?5))";

pub(super) fn inbox(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    after: Option<EventSeq>,
    limit: usize,
) -> Result<BoardReply, BoardError> {
    if !(1..=2000).contains(&limit) {
        return Err(invalid("invalid_options", "inbox limit must be 1..2000"));
    }
    task_claims::refresh_inbox_claims(tx, ctx.actor_id, ctx.now)?;
    let stored: Option<i64> = tx
        .query_row(
            "SELECT cursor_seq FROM agent_sessions WHERE actor_id=?1",
            [ctx.actor_id],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let cursor = EventSeq::new(stored.map(sqlite_u64).transpose()?.unwrap_or(0));
    let first = after.is_none() && stored.is_none();
    let start = after.unwrap_or(cursor);
    let count = if first {
        limit.min(FIRST_FEED_LIMIT)
    } else {
        limit
    };
    let order = if first { "DESC" } else { "ASC" };
    let sql = format!(
        "{EVENT_SELECT} WHERE {RELEVANT_EVENT} AND e.seq>?2 ORDER BY e.seq {order} LIMIT ?6"
    );
    let mut statement = tx.prepare(&sql).map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            ctx.actor_id,
            sql_number(start.get()),
            ctx.actor.user,
            ctx.actor.harness.as_str(),
            ctx.actor.identity(),
            sql_number(count as u64),
        ])
        .map_err(sql_error)?;
    let mut events = Vec::with_capacity(count);
    while let Some(row) = rows.next().map_err(sql_error)? {
        let to: Option<String> = row.get(4).map_err(sql_error)?;
        let plan: Option<i64> = row.get(1).map_err(sql_error)?;
        events.push(EventRecord {
            seq: EventSeq::new(row_number(row, 0).map_err(sql_error)?),
            plan: plan
                .map(sqlite_u64)
                .transpose()?
                .map(PlanId::new)
                .transpose()
                .map_err(BoardError::from)?,
            kind: row
                .get::<_, String>(2)
                .map_err(sql_error)?
                .parse()
                .map_err(BoardError::from)?,
            subject: row
                .get::<_, String>(3)
                .map_err(sql_error)?
                .parse::<BoardRef>()
                .map_err(BoardError::from)?,
            to: to
                .as_deref()
                .map(BoardRecipient::parse)
                .transpose()
                .map_err(BoardError::from)?,
            actor: actor_from_row(row, 5).map_err(sql_error)?,
            model: row.get(9).map_err(sql_error)?,
            effort: row.get(10).map_err(sql_error)?,
            summary: EntryText::new(row.get::<_, String>(11).map_err(sql_error)?)
                .map_err(BoardError::from)?,
            created_at: row.get(12).map_err(sql_error)?,
        });
    }
    if first {
        events.reverse();
    }
    let open = board_reads::open_entries(tx, &ctx.actor)?;
    Ok(BoardReply::new(
        "local",
        BoardResult::Inbox(InboxReply {
            actor: ctx.actor.clone(),
            cursor,
            events,
            open,
            latest: max_seq(tx)?,
            advancing: after.is_none(),
            wait: InboxWait::None,
        }),
    ))
}

pub(super) fn acknowledge(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    rendered_through: EventSeq,
) -> Result<BoardReply, BoardError> {
    if rendered_through.get() > max_seq(tx)?.get() {
        return Err(invalid(
            "invalid_reference",
            "inbox acknowledgement exceeds the latest event",
        ));
    }
    tx.execute(
        "UPDATE agent_sessions SET cursor_seq=max(coalesce(cursor_seq,0),?2) WHERE actor_id=?1",
        params![ctx.actor_id, sql_number(rendered_through.get())],
    )
    .map_err(sql_error)?;
    let cursor = read_cursor(tx, ctx.actor_id)?;
    Ok(BoardReply::new("local", BoardResult::Cursor(cursor)))
}

fn read_cursor(conn: &Connection, actor_id: i64) -> Result<EventSeq, BoardError> {
    let value: i64 = conn
        .query_row(
            "SELECT coalesce(cursor_seq,0) FROM agent_sessions WHERE actor_id=?1",
            [actor_id],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    Ok(EventSeq::new(sqlite_u64(value)?))
}
