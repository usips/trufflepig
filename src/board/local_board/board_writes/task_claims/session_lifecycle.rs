//! Agent-session shutdown and its claim cleanup.

use rusqlite::{OptionalExtension, Transaction, params};

use super::super::super::{
    BoardError, EntryDraft, WriteContext, insert_entry, insert_event, invalid, max_seq, row_number,
    sql_error, sql_number,
};
use super::{active_bound_worker_for_actor, end_claim};
use crate::board::board_ids::{EventSeq, TaskId};
use crate::board::board_protocol::{BoardReply, BoardResult, ClaimEndReason, WorkerEndReason};
use crate::board::board_vocabulary::{EntryKind, TaskColumn};

pub(in crate::board::local_board) fn bye(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
) -> Result<BoardReply, BoardError> {
    let ended_at: Option<i64> = tx
        .query_row(
            "SELECT ended_at FROM agent_sessions WHERE actor_id=?1",
            [ctx.actor_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| invalid("board_unavailable", "agent session row is missing"))?;
    if ended_at.is_some() {
        let previous_bye: Option<u64> = tx
            .query_row(
                "SELECT seq FROM events WHERE actor_id=?1 AND kind='bye' ORDER BY seq DESC LIMIT 1",
                [ctx.actor_id],
                |row| row_number(row, 0),
            )
            .optional()
            .map_err(sql_error)?;
        return Ok(BoardReply::new(
            "local",
            BoardResult::Cursor(EventSeq::new(previous_bye.unwrap_or(max_seq(tx)?.get()))),
        ));
    }

    let bound_worker = active_bound_worker_for_actor(tx, ctx.actor_id)?;
    if let Some(worker) = &bound_worker {
        tx.execute(
            "UPDATE claim_workers SET ended_at=?2,last_active=max(last_active,?2),end_reason=?3 WHERE id=?1 AND ended_at IS NULL",
            params![worker.id, ctx.now, WorkerEndReason::Stopped.as_str()],
        )
        .map_err(sql_error)?;
    }

    let mut tasks = Vec::new();
    let mut statement = tx
        .prepare(
            "SELECT plan_id,task_ordinal FROM claims WHERE actor_id=?1 AND ended_at IS NULL ORDER BY id",
        )
        .map_err(sql_error)?;
    let rows = statement
        .query_map([ctx.actor_id], |row| {
            Ok((row_number(row, 0)?, row_number(row, 1)?))
        })
        .map_err(sql_error)?;
    for row in rows {
        let (plan, ordinal) = row.map_err(sql_error)?;
        let plan = crate::board::board_ids::PlanId::new(plan).map_err(BoardError::from)?;
        tasks.push(TaskId::new(plan, ordinal).map_err(BoardError::from)?);
    }

    for task in &tasks {
        end_claim(tx, *task, ctx.now, ClaimEndReason::SessionEnded)?;
        tx.execute(
            "UPDATE tasks SET column_name=?3,seq=?4 WHERE plan_id=?1 AND ordinal=?2 AND column_name=?5",
            params![
                sql_number(task.plan.get()),
                sql_number(task.ordinal),
                TaskColumn::Todo.as_str(),
                sql_number(ctx.seq.get()),
                TaskColumn::Doing.as_str()
            ],
        )
        .map_err(sql_error)?;
    }
    tx.execute(
        "UPDATE agent_sessions SET ended_at=?2,last_seen=max(last_seen,?2) WHERE actor_id=?1 AND ended_at IS NULL",
        params![ctx.actor_id, ctx.now],
    )
    .map_err(sql_error)?;

    let mut summary = String::from("ended session");
    if let Some(worker) = &bound_worker {
        summary.push_str(&format!("; stopped worker {}", worker.record.address));
    }
    if !tasks.is_empty() {
        summary.push_str(&format!("; ended {} lead claim(s)", tasks.len()));
    }
    let entry = insert_entry(
        tx,
        ctx,
        &EntryDraft {
            plan_id: None,
            kind: EntryKind::Bye,
            body: summary.clone(),
            to_whom: None,
            supersedes: None,
            repo_key: None,
            state: None,
        },
    )?;
    insert_event(
        tx,
        ctx,
        None,
        EntryKind::Bye,
        &entry.to_string(),
        None,
        &summary,
    )?;
    Ok(BoardReply::new("local", BoardResult::Cursor(ctx.seq)))
}
