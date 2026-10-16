//! Append-only task progress facts and review verdicts.

use rusqlite::{Transaction, params};

use super::super::{BoardError, WriteContext, invalid, sql_error, sql_number};
use crate::board::board_ids::{EntryId, TaskId};
use crate::board::board_protocol::{GateStatus, ProgressInput, ReviewVerdict, WorkerAddress};
use crate::board::board_vocabulary::EntryKind;
use crate::identity::GitOid;
use authorization::{
    current_claim, require_progress_author, require_verdict_author, task_entry_worker,
};
use validation::validate_progress;

mod authorization;
mod validation;

pub(in crate::board::local_board) fn write_progress(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    entry: EntryId,
    task: TaskId,
    worker: Option<&WorkerAddress>,
    input: &ProgressInput,
    trusted_head: Option<&GitOid>,
) -> Result<(), BoardError> {
    if worker.is_some_and(|worker| worker.task != task) {
        return Err(invalid(
            "invalid_worker",
            "progress worker belongs to a different task",
        ));
    }
    let entry_worker = task_entry_worker(tx, entry, task, EntryKind::Progress)?;
    let (claim, claim_id) = current_claim(tx, ctx, task)?;
    let worker_id = require_progress_author(tx, ctx, task, &claim, claim_id, entry_worker, worker)?;
    let fields = validate_progress(input, trusted_head, ctx)?;

    tx.execute(
        concat!(
            "INSERT INTO progress_facts(entry_id,claim_id,worker_id,head_oid,head_verified,",
            "step_done,step_total,tests_passed,tests_failed,eta_at,updated_at) ",
            "VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)"
        ),
        params![
            sql_number(entry.get()),
            claim_id,
            worker_id,
            fields.head_oid,
            fields.head_verified,
            fields.step.map(|(current, _)| current),
            fields.step.map(|(_, total)| total),
            fields.tests.map(|(passed, _)| passed),
            fields.tests.map(|(_, failed)| failed),
            fields.eta_at,
            ctx.now,
        ],
    )
    .map_err(sql_error)?;

    for gate in &input.gates {
        tx.execute(
            "INSERT INTO progress_gates(entry_id,name,result) VALUES(?1,?2,?3)",
            params![
                sql_number(entry.get()),
                gate.name,
                match gate.status {
                    GateStatus::Pass => "pass",
                    GateStatus::Fail => "fail",
                    GateStatus::Running => "running",
                },
            ],
        )
        .map_err(sql_error)?;
    }
    Ok(())
}

pub(in crate::board::local_board) fn write_verdict(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    entry: EntryId,
    task: TaskId,
    verdict: &ReviewVerdict,
) -> Result<(), BoardError> {
    let entry_worker = task_entry_worker(tx, entry, task, EntryKind::Review)?;
    let (claim, claim_id) = current_claim(tx, ctx, task)?;
    require_verdict_author(tx, ctx, task, &claim, claim_id, entry_worker)?;

    let (kind, count) = match verdict {
        ReviewVerdict::Clean => ("clean", None),
        ReviewVerdict::Findings(count) => ("findings", Some(i64::from(*count))),
        ReviewVerdict::Blocking(count) => ("blocking", Some(i64::from(*count))),
    };
    let already_recorded: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM review_verdicts WHERE entry_id=?1)",
            [sql_number(entry.get())],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    if already_recorded {
        return Err(invalid(
            "invalid_state",
            format!("review entry {entry} already has a verdict"),
        ));
    }
    tx.execute(
        "INSERT INTO review_verdicts(entry_id,verdict,count) VALUES(?1,?2,?3)",
        params![sql_number(entry.get()), kind, count],
    )
    .map_err(sql_error)?;
    Ok(())
}

#[cfg(test)]
mod tests;
