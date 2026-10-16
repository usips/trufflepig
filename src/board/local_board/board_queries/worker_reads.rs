//! Bounded worker registry reads for task and claim projections.

use rusqlite::{Connection, OptionalExtension, Row, params};

use super::super::{
    BoardError, delegated_actor_from_row, invalid, row_number, sql_error, sql_number,
};
use crate::board::board_ids::{EntryId, TaskId};
use crate::board::board_protocol::{
    WorkerAddress, WorkerEndReason, WorkerHandle, WorkerRecord, WorkerRole,
};
use crate::board::board_vocabulary::EntryText;

pub(in crate::board::local_board) const MAX_CLAIM_WORKERS: usize = 20;

#[derive(Debug)]
pub(in crate::board::local_board) struct StoredWorker {
    pub id: i64,
    pub record: WorkerRecord,
    pub claim_id: i64,
    pub claim_entry: EntryId,
    pub actor_id: Option<i64>,
    pub ended_at: Option<i64>,
}

#[derive(Debug)]
pub(in crate::board::local_board) struct ActiveWorkerBinding {
    pub worker_id: i64,
    pub claim_id: i64,
    pub address: WorkerAddress,
    pub claim_entry: EntryId,
}

const WORKER_SELECT: &str = concat!(
    "SELECT w.id,w.claim_id,c.entry_id,w.entry_id,w.plan_id,w.task_ordinal,w.handle,",
    "w.actor_id,a.user,a.host,a.harness,a.session,w.role,w.model,w.effort,w.scope,w.files_json,",
    "w.added_at,w.last_active,w.ended_at,w.end_reason,",
    "(SELECT ee.body FROM entries ee WHERE ee.worker_id=w.id AND ee.kind='worker_end' ",
    "ORDER BY ee.seq DESC LIMIT 1) ",
    "FROM claim_workers w JOIN claims c ON c.id=w.claim_id ",
    "LEFT JOIN actors a ON a.id=w.actor_id"
);

pub(in crate::board::local_board) fn workers_for_claim(
    conn: &Connection,
    task: TaskId,
    claim_entry: EntryId,
    _now: i64,
    _ttl: i64,
) -> Result<(Vec<WorkerRecord>, usize), BoardError> {
    let claim_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM claims WHERE plan_id=?1 AND task_ordinal=?2 AND entry_id=?3",
            params![
                sql_number(task.plan.get()),
                sql_number(task.ordinal),
                sql_number(claim_entry.get())
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    let Some(claim_id) = claim_id else {
        return Ok((Vec::new(), 0));
    };
    let total: usize = conn
        .query_row(
            "SELECT count(*) FROM claim_workers WHERE claim_id=?1",
            [claim_id],
            |row| row_number(row, 0),
        )
        .map_err(sql_error)?
        .try_into()
        .map_err(|_| invalid("board_unavailable", "worker count exceeds address space"))?;
    let sql = format!(
        "{WORKER_SELECT} WHERE w.claim_id=?1 ORDER BY (w.ended_at IS NOT NULL),w.last_active DESC,w.id DESC LIMIT ?2"
    );
    let mut statement = conn.prepare(&sql).map_err(sql_error)?;
    let mut rows = statement
        .query(params![claim_id, MAX_CLAIM_WORKERS as i64])
        .map_err(sql_error)?;
    let mut workers = Vec::with_capacity(total.min(MAX_CLAIM_WORKERS));
    while let Some(row) = rows.next().map_err(sql_error)? {
        workers.push(worker_from_row(row, task, claim_entry, conn)?.record);
    }
    let omitted = total.saturating_sub(workers.len());
    Ok((workers, omitted))
}

pub(in crate::board::local_board) fn worker_for_address(
    conn: &Connection,
    address: &WorkerAddress,
) -> Result<Option<StoredWorker>, BoardError> {
    let sql = format!("{WORKER_SELECT} WHERE w.plan_id=?1 AND w.task_ordinal=?2 AND w.handle=?3");
    let mut statement = conn.prepare(&sql).map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            sql_number(address.task.plan.get()),
            sql_number(address.task.ordinal),
            address.handle.as_str()
        ])
        .map_err(sql_error)?;
    rows.next()
        .map_err(sql_error)?
        .map(|row| {
            let claim_entry =
                EntryId::new(row_number(row, 2).map_err(sql_error)?).map_err(BoardError::from)?;
            worker_from_row(row, address.task, claim_entry, conn)
        })
        .transpose()
}

pub(in crate::board::local_board) fn active_bound_worker_for_actor(
    conn: &Connection,
    actor_id: i64,
) -> Result<Option<StoredWorker>, BoardError> {
    let sql = format!(
        "{WORKER_SELECT} WHERE w.actor_id=?1 AND w.ended_at IS NULL AND EXISTS(SELECT 1 FROM claims ac WHERE ac.id=w.claim_id AND ac.ended_at IS NULL) ORDER BY w.added_at DESC LIMIT 1"
    );
    let mut statement = conn.prepare(&sql).map_err(sql_error)?;
    let mut rows = statement.query([actor_id]).map_err(sql_error)?;
    rows.next()
        .map_err(sql_error)?
        .map(|row| stored_worker_from_row(row, conn))
        .transpose()
}

pub(in crate::board::local_board) fn active_worker_binding(
    conn: &Connection,
    actor_id: i64,
    task: TaskId,
) -> Result<Option<ActiveWorkerBinding>, BoardError> {
    let row: Option<(i64, i64, String, u64)> = conn
        .query_row(
            concat!(
                "SELECT w.id,w.claim_id,w.handle,c.entry_id FROM claim_workers w ",
                "JOIN claims c ON c.id=w.claim_id ",
                "WHERE w.actor_id=?1 AND w.plan_id=?2 AND w.task_ordinal=?3 ",
                "AND w.ended_at IS NULL AND c.ended_at IS NULL"
            ),
            params![
                actor_id,
                sql_number(task.plan.get()),
                sql_number(task.ordinal)
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row_number(row, 3)?)),
        )
        .optional()
        .map_err(sql_error)?;
    row.map(|(worker_id, claim_id, handle, claim_entry)| {
        let handle = WorkerHandle::parse(&handle).map_err(BoardError::from)?;
        Ok(ActiveWorkerBinding {
            worker_id,
            claim_id,
            address: WorkerAddress::new(task, handle).map_err(BoardError::from)?,
            claim_entry: EntryId::new(claim_entry).map_err(BoardError::from)?,
        })
    })
    .transpose()
}

fn worker_from_row(
    row: &Row<'_>,
    task: TaskId,
    claim_entry: EntryId,
    conn: &Connection,
) -> Result<StoredWorker, BoardError> {
    let stored = stored_worker_from_row(row, conn)?;
    if stored.record.address.task != task || stored.claim_entry != claim_entry {
        return Err(invalid(
            "board_unavailable",
            "worker row does not match its task claim",
        ));
    }
    Ok(stored)
}

fn stored_worker_from_row(row: &Row<'_>, conn: &Connection) -> Result<StoredWorker, BoardError> {
    let id = row.get(0).map_err(sql_error)?;
    let claim_id = row.get(1).map_err(sql_error)?;
    let claim_entry =
        EntryId::new(row_number(row, 2).map_err(sql_error)?).map_err(BoardError::from)?;
    let entry = EntryId::new(row_number(row, 3).map_err(sql_error)?).map_err(BoardError::from)?;
    let plan = crate::board::board_ids::PlanId::new(row_number(row, 4).map_err(sql_error)?)
        .map_err(BoardError::from)?;
    let ordinal = row_number(row, 5).map_err(sql_error)?;
    let task = TaskId::new(plan, ordinal).map_err(BoardError::from)?;
    let handle: String = row.get(6).map_err(sql_error)?;
    let handle = WorkerHandle::parse(&handle).map_err(BoardError::from)?;
    let address = WorkerAddress::new(task, handle).map_err(BoardError::from)?;
    let actor_id: Option<i64> = row.get(7).map_err(sql_error)?;
    let actor = delegated_actor_from_row(row, 8).map_err(sql_error)?;
    let role: String = row.get(12).map_err(sql_error)?;
    let role = WorkerRole::parse(&role).map_err(BoardError::from)?;
    let model: String = row.get(13).map_err(sql_error)?;
    let effort: Option<String> = row.get(14).map_err(sql_error)?;
    let scope: String = row.get(15).map_err(sql_error)?;
    let scope = EntryText::new(scope).map_err(BoardError::from)?;
    let files_json: String = row.get(16).map_err(sql_error)?;
    let files = serde_json::from_str(&files_json).map_err(|error| {
        invalid(
            "board_unavailable",
            format!("invalid worker files: {error}"),
        )
    })?;
    let started_at: i64 = row.get(17).map_err(sql_error)?;
    let last_active: i64 = row.get(18).map_err(sql_error)?;
    let ended_at: Option<i64> = row.get(19).map_err(sql_error)?;
    let end_reason: Option<String> = row.get(20).map_err(sql_error)?;
    let state = end_reason
        .as_deref()
        .map(WorkerEndReason::parse)
        .transpose()
        .map_err(BoardError::from)?;
    let end_body: Option<String> = row.get(21).map_err(sql_error)?;
    let note_prefix = state
        .as_ref()
        .map(|state| format!("ended worker {address} as {}: ", state.as_str()));
    let note = end_body
        .zip(note_prefix)
        .and_then(|(body, prefix)| body.strip_prefix(&prefix).map(str::to_owned))
        .map(EntryText::new)
        .transpose()
        .map_err(BoardError::from)?;
    let progress = super::progress_reads::progress_for_claim(conn, claim_entry, Some(&address))?;
    let verdict = super::progress_reads::latest_worker_verdict(conn, &address)?;
    Ok(StoredWorker {
        id,
        claim_id,
        claim_entry,
        actor_id,
        ended_at,
        record: WorkerRecord {
            address,
            claim: claim_entry,
            entry,
            actor,
            role,
            model,
            effort,
            scope,
            files,
            state,
            added_at: started_at,
            last_active,
            ended_at,
            note,
            progress,
            verdict,
        },
    })
}
