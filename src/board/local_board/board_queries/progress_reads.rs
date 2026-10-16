//! Claim-scoped progress snapshots and durable review verdict reads.

use rusqlite::{Connection, OptionalExtension, params};

use super::super::{BoardError, row_number, sql_error, sql_number};
use crate::board::board_ids::{EntryId, PlanId, TaskId};
use crate::board::board_protocol::{ProgressEvidence, WorkerAddress};

mod progress_value_reads;
mod verdict_reads;

pub(in crate::board::local_board) use verdict_reads::{latest_worker_verdict, verdict_for_entry};

pub(in crate::board::local_board) fn progress_for_claim(
    conn: &Connection,
    claim_entry: EntryId,
    worker: Option<&WorkerAddress>,
) -> Result<Option<ProgressEvidence>, BoardError> {
    let claim = conn
        .query_row(
            "SELECT id,plan_id,task_ordinal FROM claims WHERE entry_id=?1",
            [sql_number(claim_entry.get())],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row_number(row, 1)?,
                    row_number(row, 2)?,
                ))
            },
        )
        .optional()
        .map_err(sql_error)?;
    let Some((claim_id, plan_number, task_ordinal)) = claim else {
        return Ok(None);
    };
    let task = TaskId::new(
        PlanId::new(plan_number).map_err(BoardError::from)?,
        task_ordinal,
    )
    .map_err(BoardError::from)?;
    let worker_id = match worker {
        Some(address) if address.task != task => return Ok(None),
        Some(address) => conn
            .query_row(
                concat!(
                    "SELECT id FROM claim_workers WHERE claim_id=?1 AND plan_id=?2 ",
                    "AND task_ordinal=?3 AND handle=?4"
                ),
                params![
                    claim_id,
                    sql_number(task.plan.get()),
                    sql_number(task.ordinal),
                    address.handle.as_str(),
                ],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(sql_error)?,
        None => None,
    };
    if worker.is_some() && worker_id.is_none() {
        return Ok(None);
    }

    let head = progress_value_reads::latest_head(conn, claim_id, worker_id)?;
    let step = progress_value_reads::latest_step(conn, claim_id, worker_id)?;
    let tests = progress_value_reads::latest_tests(conn, claim_id, worker_id)?;
    let eta_at = progress_value_reads::latest_eta(conn, claim_id, worker_id)?;
    let gates = progress_value_reads::latest_gates(conn, claim_id, worker_id)?;
    if head.is_none() && step.is_none() && tests.is_none() && eta_at.is_none() && gates.is_empty() {
        return Ok(None);
    }
    Ok(Some(ProgressEvidence {
        claim: claim_entry,
        worker: worker.cloned(),
        head,
        step,
        tests,
        gates,
        eta_at,
    }))
}

#[cfg(test)]
mod tests;
