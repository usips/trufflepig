//! Agent-session shutdown and its claim cleanup.

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::super::super::{
    BoardError, EntryDraft, WriteContext, insert_entry, insert_event, lookup_actor, row_number,
    sql_error, sql_number,
};
use super::{active_bound_worker_for_actor, end_claim};
use crate::board_actor::BoardActor;
use crate::board_ids::{EntryId, EventSeq, TaskId};
use crate::board_protocol::{
    BoardReply, BoardResult, ByeOutcome, ByeReply, ClaimEndReason, WorkerEndReason,
};
use crate::board_vocabulary::{EntryKind, TaskColumn};

/// The reply to a bye that writes nothing: the identity has no recorded session,
/// or its session already ended. `None` means a live session must end.
pub(in crate::local_board) fn settled_bye(
    conn: &Connection,
    actor: &BoardActor,
) -> Result<Option<BoardReply>, BoardError> {
    let Some(actor_id) = lookup_actor(conn, actor)? else {
        return Ok(Some(bye_reply(ByeOutcome::NoSession, None)));
    };
    let ended_at: Option<Option<i64>> = conn
        .query_row(
            "SELECT ended_at FROM agent_sessions WHERE actor_id=?1",
            [actor_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    match ended_at {
        None => Ok(Some(bye_reply(ByeOutcome::NoSession, None))),
        Some(None) => Ok(None),
        Some(Some(_)) => {
            let original = conn
                .query_row(
                    "SELECT id,seq FROM entries WHERE actor_id=?1 AND kind=?2 ORDER BY seq DESC LIMIT 1",
                    params![actor_id, EntryKind::Bye.as_str()],
                    |row| Ok((row_number(row, 0)?, row_number(row, 1)?)),
                )
                .optional()
                .map_err(sql_error)?;
            let original = original
                .map(|(entry, seq)| Ok::<_, BoardError>((EntryId::new(entry)?, EventSeq::new(seq))))
                .transpose()?;
            Ok(Some(bye_reply(ByeOutcome::AlreadyEnded, original)))
        }
    }
}

fn bye_reply(outcome: ByeOutcome, bye: Option<(EntryId, EventSeq)>) -> BoardReply {
    BoardReply::new(
        "local",
        BoardResult::Bye(ByeReply {
            outcome,
            entry: bye.map(|(entry, _)| entry),
            seq: bye.map(|(_, seq)| seq),
        }),
    )
}

/// Ends the caller's live session; an ended session reports its original bye.
pub(in crate::local_board) fn bye(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
) -> Result<BoardReply, BoardError> {
    if let Some(settled) = settled_bye(tx, &ctx.actor)? {
        return Ok(settled);
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
        let plan = crate::board_ids::PlanId::new(plan).map_err(BoardError::from)?;
        tasks.push(TaskId::new(plan, ordinal).map_err(BoardError::from)?);
    }

    for task in &tasks {
        end_claim(tx, ctx, *task, ClaimEndReason::SessionEnded)?;
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
    Ok(bye_reply(ByeOutcome::Ended, Some((entry, ctx.seq))))
}
