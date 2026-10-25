use crate::board::board_protocol::BoardErrorCode;
use rusqlite::{OptionalExtension, Transaction, params};

use super::super::super::{BoardError, WriteContext, invalid, sql_error, sql_number};
use super::super::task_claims::{StoredClaim, active_claim};
use crate::board::board_ids::{EntryId, TaskId};
use crate::board::board_protocol::WorkerAddress;
use crate::board::board_vocabulary::EntryKind;

pub(super) fn current_claim(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    task: TaskId,
) -> Result<(StoredClaim, i64), BoardError> {
    let claim = active_claim(tx, task, ctx.now, ctx.claim_ttl_secs)?.ok_or_else(|| {
        invalid(
            BoardErrorCode::InvalidState,
            format!("{task} has no current claim for structured evidence"),
        )
    })?;
    if claim.record.stale {
        return Err(invalid(
            BoardErrorCode::InvalidState,
            format!("{task} current claim is stale"),
        ));
    }
    let claim_id = tx
        .query_row(
            "SELECT id FROM claims WHERE entry_id=?1 AND ended_at IS NULL",
            [sql_number(claim.record.entry.get())],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| {
            invalid(
                BoardErrorCode::InvalidState,
                format!("{task} claim has ended"),
            )
        })?;
    Ok((claim, claim_id))
}

pub(super) fn task_entry_worker(
    tx: &Transaction<'_>,
    entry: EntryId,
    task: TaskId,
    expected_kind: EntryKind,
) -> Result<Option<i64>, BoardError> {
    let task_reference = task.to_string();
    let row = tx
        .query_row(
            concat!(
                "SELECT e.kind,e.worker_id FROM entries e WHERE e.id=?1 AND e.plan_id=?2 ",
                "AND EXISTS(SELECT 1 FROM entry_refs r WHERE r.entry_id=e.id AND r.target=?3)"
            ),
            params![
                sql_number(entry.get()),
                sql_number(task.plan.get()),
                task_reference,
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?)),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| {
            invalid(
                BoardErrorCode::InvalidReference,
                format!("{entry} is not an entry on {task}"),
            )
        })?;
    if row.0 != expected_kind.as_str() {
        return Err(invalid(
            BoardErrorCode::InvalidReference,
            format!(
                "structured evidence requires a {} entry",
                expected_kind.as_str()
            ),
        ));
    }
    Ok(row.1)
}

pub(super) fn require_progress_author(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    task: TaskId,
    claim: &StoredClaim,
    claim_id: i64,
    entry_worker_id: Option<i64>,
    worker: Option<&WorkerAddress>,
) -> Result<Option<i64>, BoardError> {
    match worker {
        Some(worker) => {
            let worker_id = active_worker_id(tx, ctx, task, claim_id, worker.handle.as_str())?;
            if entry_worker_id != Some(worker_id) {
                return Err(invalid(
                    BoardErrorCode::InvalidActor,
                    "progress entry worker binding does not match its active worker",
                ));
            }
            Ok(Some(worker_id))
        }
        None => {
            if entry_worker_id.is_some() || claim.actor_id != ctx.actor_id {
                return Err(invalid(
                    BoardErrorCode::InvalidActor,
                    "structured task evidence requires the active claim holder or bound worker",
                ));
            }
            Ok(None)
        }
    }
}

pub(super) fn require_verdict_author(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    task: TaskId,
    claim: &StoredClaim,
    claim_id: i64,
    entry_worker_id: Option<i64>,
) -> Result<(), BoardError> {
    match entry_worker_id {
        Some(worker_id) => {
            let worker = tx
                .query_row(
                    concat!(
                        "SELECT claim_id,actor_id,ended_at FROM claim_workers WHERE id=?1 ",
                        "AND plan_id=?2 AND task_ordinal=?3"
                    ),
                    params![
                        worker_id,
                        sql_number(task.plan.get()),
                        sql_number(task.ordinal),
                    ],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, Option<i64>>(1)?,
                            row.get::<_, Option<i64>>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(sql_error)?
                .ok_or_else(|| {
                    invalid(
                        BoardErrorCode::InvalidWorker,
                        "review worker binding is missing",
                    )
                })?;
            if worker.0 != claim_id || worker.1 != Some(ctx.actor_id) || worker.2.is_some() {
                return Err(invalid(
                    BoardErrorCode::InvalidActor,
                    "review verdict requires the active worker bound to this claim",
                ));
            }
            Ok(())
        }
        None if claim.actor_id == ctx.actor_id => Ok(()),
        None => Err(invalid(
            BoardErrorCode::InvalidActor,
            "structured task evidence requires the active claim holder or bound worker",
        )),
    }
}

pub(super) fn active_worker_id(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    task: TaskId,
    claim_id: i64,
    handle: &str,
) -> Result<i64, BoardError> {
    let worker = tx
        .query_row(
            concat!(
                "SELECT id,claim_id,actor_id,ended_at FROM claim_workers ",
                "WHERE plan_id=?1 AND task_ordinal=?2 AND handle=?3"
            ),
            params![
                sql_number(task.plan.get()),
                sql_number(task.ordinal),
                handle
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            },
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| {
            invalid(
                BoardErrorCode::InvalidWorker,
                format!("unknown worker {task}/{handle}"),
            )
        })?;
    if worker.1 != claim_id || worker.2 != Some(ctx.actor_id) || worker.3.is_some() {
        return Err(invalid(
            BoardErrorCode::InvalidActor,
            "progress requires the active worker bound to this claim",
        ));
    }
    Ok(worker.0)
}
