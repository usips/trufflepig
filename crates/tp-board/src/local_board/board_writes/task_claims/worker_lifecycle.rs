//! Claim-scoped worker registration, binding, and activity.

use crate::board::board_protocol::BoardErrorCode;
use rusqlite::{OptionalExtension, Transaction, params};

use super::super::super::{
    BoardError, EntryDraft, WriteContext, actor_from_row, insert_entry, insert_event, invalid,
    row_number, sql_error, sql_number,
};
use super::{
    active_bound_worker_for_actor, active_claim, active_worker_binding, worker_for_address,
};
use crate::board::board_actor::{BoardActor, BoardRecipient};
use crate::board::board_ids::{EntryId, TaskId};
use crate::board::board_protocol::{
    AuthorityRole, AuthoritySource, BoardReply, WORKER_FILE_LIMIT, WORKER_FILE_PATH_LIMIT,
    WorkerAddress, WorkerEndReason, WorkerHandle, WorkerRole,
};
use crate::board::board_vocabulary::{ENTRY_TEXT_LIMIT, EntryKind, EntryText};

pub(in crate::board::local_board) fn add_worker(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    task: TaskId,
    handle: &WorkerHandle,
    role: WorkerRole,
    model: &str,
    effort: Option<&str>,
    scope: &EntryText,
    files: &[String],
) -> Result<BoardReply, BoardError> {
    let holder = current_claim(tx, ctx, task)?;
    let write_ctx = worker_manager_context(tx, ctx, task, &holder)?;
    let handle = WorkerHandle::parse(handle.as_str()).map_err(BoardError::from)?;
    validate_worker_files(files)?;
    if model.trim().is_empty() || model.len() > 256 || model.chars().any(char::is_control) {
        return Err(invalid(
            BoardErrorCode::InvalidOptions,
            "worker model must contain 1..256 bytes without control characters",
        ));
    }
    if let Some(effort) = effort
        && (effort.trim().is_empty() || effort.len() > 128 || effort.chars().any(char::is_control))
    {
        return Err(invalid(
            BoardErrorCode::InvalidOptions,
            "worker effort must contain 1..128 bytes without control characters",
        ));
    }
    let duplicate: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM claim_workers WHERE plan_id=?1 AND task_ordinal=?2 AND handle=?3)",
            params![sql_number(task.plan.get()), sql_number(task.ordinal), handle.as_str()],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    if duplicate {
        return Err(invalid(
            BoardErrorCode::InvalidWorker,
            format!("worker handle {} was already used for {task}", handle),
        ));
    }
    let claim_id: i64 = tx
        .query_row(
            "SELECT id FROM claims WHERE plan_id=?1 AND task_ordinal=?2 AND entry_id=?3 AND ended_at IS NULL",
            params![
                sql_number(task.plan.get()),
                sql_number(task.ordinal),
                sql_number(holder.record.entry.get())
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| invalid(BoardErrorCode::InvalidState, format!("{task} has no current claim")))?;
    let address = WorkerAddress::new(task, handle).map_err(BoardError::from)?;
    let summary = format!("added worker {address} ({})", role.as_str());
    let entry = insert_entry(
        tx,
        &write_ctx,
        &EntryDraft {
            plan_id: Some(task.plan),
            kind: EntryKind::WorkerAdd,
            body: summary.clone(),
            to_whom: None,
            supersedes: None,
            repo_key: None,
            state: None,
        },
    )?;
    let files_json = serde_json::to_string(files).map_err(|error| {
        invalid(
            BoardErrorCode::InvalidOptions,
            format!("cannot encode worker files: {error}"),
        )
    })?;
    let worker_id = tx
        .query_row(
            concat!(
                "INSERT INTO claim_workers(claim_id,entry_id,plan_id,task_ordinal,handle,model,effort,",
                "role,scope,files_json,added_at,last_active) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?11) RETURNING id"
            ),
            params![
                claim_id,
                sql_number(entry.get()),
                sql_number(task.plan.get()),
                sql_number(task.ordinal),
                address.handle.as_str(),
                model,
                effort,
                role.as_str(),
                scope.as_str(),
                files_json,
                ctx.now
            ],
            |row| row_number(row, 0),
        )
        .map_err(sql_error)?;
    tx.execute(
        "UPDATE entries SET worker_id=?2 WHERE id=?1",
        params![sql_number(entry.get()), sql_number(worker_id)],
    )
    .map_err(sql_error)?;
    insert_event(
        tx,
        &write_ctx,
        Some(task.plan),
        EntryKind::WorkerAdd,
        &address.to_string(),
        None,
        &summary,
    )?;
    Ok(write_ctx.change_reply(entry, Some(task.plan), None, Some(task)))
}

pub(in crate::board::local_board) fn end_worker(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    address: &WorkerAddress,
    state: WorkerEndReason,
    note: Option<&EntryText>,
) -> Result<BoardReply, BoardError> {
    address.validate().map_err(BoardError::from)?;
    if state == WorkerEndReason::ClaimEnded {
        return Err(invalid(
            BoardErrorCode::InvalidOptions,
            "claim_ended is reserved for claim lifecycle changes",
        ));
    }
    let worker = worker_for_address(tx, address)?.ok_or_else(|| {
        invalid(
            BoardErrorCode::InvalidReference,
            format!("unknown worker {address}"),
        )
    })?;
    let holder = current_claim(tx, ctx, address.task)?;
    if holder.record.entry != worker.claim_entry {
        return Err(invalid(
            BoardErrorCode::InvalidState,
            format!("worker {address} belongs to an ended or replaced claim"),
        ));
    }
    let write_ctx = worker_manager_context(tx, ctx, address.task, &holder)?;
    if worker.ended_at.is_some() {
        return Err(invalid(
            BoardErrorCode::InvalidState,
            format!("worker {address} has already ended"),
        ));
    }
    let summary = format!("ended worker {address} as {}", state.as_str());
    if note.is_some_and(|note| summary.len() + 2 + note.as_str().len() > ENTRY_TEXT_LIMIT) {
        return Err(invalid(
            BoardErrorCode::InvalidOptions,
            format!(
                "worker end note must fit within {ENTRY_TEXT_LIMIT} bytes with its audit summary"
            ),
        ));
    }
    let entry = insert_entry(
        tx,
        &write_ctx,
        &EntryDraft {
            plan_id: Some(address.task.plan),
            kind: EntryKind::WorkerEnd,
            body: note.map_or_else(
                || summary.clone(),
                |note| format!("{summary}: {}", note.as_str()),
            ),
            to_whom: None,
            supersedes: None,
            repo_key: None,
            state: None,
        },
    )?;
    let changed = tx
        .execute(
            "UPDATE claim_workers SET ended_at=?2,last_active=max(last_active,?2),end_reason=?3 WHERE id=?1 AND ended_at IS NULL AND claim_id=?4",
            params![worker.id, ctx.now, state.as_str(), worker.claim_id],
        )
        .map_err(sql_error)?;
    if changed != 1 {
        return Err(invalid(
            BoardErrorCode::InvalidState,
            format!("worker {address} has already ended"),
        ));
    }
    tx.execute(
        "UPDATE entries SET worker_id=?2 WHERE id=?1",
        params![sql_number(entry.get()), worker.id],
    )
    .map_err(sql_error)?;
    insert_event(
        tx,
        &write_ctx,
        Some(address.task.plan),
        EntryKind::WorkerEnd,
        &address.to_string(),
        None,
        &summary,
    )?;
    Ok(write_ctx.change_reply(entry, Some(address.task.plan), None, Some(address.task)))
}

pub(in crate::board::local_board) fn bind_worker(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    address: &WorkerAddress,
    model: &str,
    effort: Option<&str>,
) -> Result<(), BoardError> {
    address.validate().map_err(BoardError::from)?;
    let holder = current_claim(tx, ctx, address.task)?;
    if ctx.actor.user != holder.record.actor.user || ctx.actor.host != holder.record.actor.host {
        return Err(invalid(
            BoardErrorCode::InvalidActor,
            "worker session must match the claim holder's user and host",
        ));
    }
    let worker = worker_for_address(tx, address)?.ok_or_else(|| {
        invalid(
            BoardErrorCode::InvalidReference,
            format!("unknown worker {address}"),
        )
    })?;
    if worker.claim_entry != holder.record.entry {
        return Err(invalid(
            BoardErrorCode::InvalidState,
            format!("worker {address} belongs to an ended or replaced claim"),
        ));
    }
    if worker.ended_at.is_some() {
        return Err(invalid(
            BoardErrorCode::InvalidState,
            format!("worker {address} has ended"),
        ));
    }
    if worker.record.model != model || worker.record.effort.as_deref() != effort {
        return Err(invalid(
            BoardErrorCode::InvalidActor,
            format!("worker {address} requires its declared model and effort"),
        ));
    }
    if let Some(bound_actor) = worker.actor_id {
        if bound_actor == ctx.actor_id {
            refresh_worker_and_parent(tx, worker.id, worker.claim_id, ctx.now)?;
            return Ok(());
        }
        return Err(invalid(
            BoardErrorCode::InvalidActor,
            format!("worker {address} is already bound to another session"),
        ));
    }
    if let Some(existing) = active_bound_worker_for_actor(tx, ctx.actor_id)? {
        return Err(invalid(
            BoardErrorCode::InvalidActor,
            format!(
                "this session is already bound to worker {}",
                existing.record.address
            ),
        ));
    }
    let changed = tx
        .execute(
            "UPDATE claim_workers SET actor_id=?2,last_active=max(last_active,?3) WHERE id=?1 AND actor_id IS NULL AND ended_at IS NULL AND claim_id=?4",
            params![worker.id, ctx.actor_id, ctx.now, worker.claim_id],
        )
        .map_err(sql_error)?;
    if changed != 1 {
        return Err(invalid(
            BoardErrorCode::InvalidActor,
            format!("worker {address} was bound by another session"),
        ));
    }
    refresh_worker_and_parent(tx, worker.id, worker.claim_id, ctx.now)?;
    Ok(())
}

fn refresh_worker_and_parent(
    tx: &Transaction<'_>,
    worker_id: i64,
    claim_id: i64,
    now: i64,
) -> Result<(), BoardError> {
    tx.execute(
        "UPDATE claim_workers SET last_active=max(last_active,?3) WHERE id=?1 AND claim_id=?2 AND ended_at IS NULL",
        params![worker_id, claim_id, now],
    )
    .map_err(sql_error)?;
    tx.execute(
        "UPDATE claims SET last_active=max(last_active,?2) WHERE id=?1 AND ended_at IS NULL",
        params![claim_id, now],
    )
    .map_err(sql_error)?;
    Ok(())
}

pub(in crate::board::local_board) fn attach_worker_post(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    task: TaskId,
    entry: EntryId,
) -> Result<(), BoardError> {
    let Some(binding) = active_worker_binding(tx, ctx.actor_id, task)? else {
        return Ok(());
    };
    let holder = active_claim(tx, task, ctx.now, ctx.claim_ttl_secs)?;
    let Some(holder) = holder else {
        return Ok(());
    };
    if holder.record.entry != binding.claim_entry {
        return Ok(());
    }
    tx.execute(
        "UPDATE entries SET worker_id=?2 WHERE id=?1 AND worker_id IS NULL",
        params![sql_number(entry.get()), binding.worker_id],
    )
    .map_err(sql_error)?;
    tx.execute(
        "UPDATE claim_workers SET last_active=max(last_active,?2) WHERE id=?1 AND ended_at IS NULL AND claim_id=?3",
        params![binding.worker_id, ctx.now, binding.claim_id],
    )
    .map_err(sql_error)?;
    tx.execute(
        "UPDATE claims SET last_active=max(last_active,?2) WHERE entry_id=?1 AND ended_at IS NULL",
        params![sql_number(binding.claim_entry.get()), ctx.now],
    )
    .map_err(sql_error)?;
    Ok(())
}

/// Ends the live claim's workers as `claim_ended`. Each bound worker also gets a `worker_end`
/// entry addressed to its session, which `board pending` delivers after the lease closes.
pub(in crate::board::local_board) fn end_workers_for_claim(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    task: TaskId,
) -> Result<(), BoardError> {
    let bound = bound_workers_for_live_claim(tx, task)?;
    tx.execute(
        concat!(
            "UPDATE claim_workers SET ended_at=?3,last_active=max(last_active,?3),end_reason='claim_ended' ",
            "WHERE claim_id=(SELECT id FROM claims WHERE plan_id=?1 AND task_ordinal=?2 AND ended_at IS NULL) ",
            "AND ended_at IS NULL"
        ),
        params![sql_number(task.plan.get()), sql_number(task.ordinal), ctx.now],
    )
    .map_err(sql_error)?;
    for (worker_id, handle, actor) in bound {
        let address = WorkerAddress::new(
            task,
            WorkerHandle::parse(&handle).map_err(BoardError::from)?,
        )
        .map_err(BoardError::from)?;
        let entry = insert_entry(
            tx,
            ctx,
            &EntryDraft {
                plan_id: Some(task.plan),
                kind: EntryKind::WorkerEnd,
                body: format!(
                    "ended worker {address} as {}",
                    WorkerEndReason::ClaimEnded.as_str()
                ),
                to_whom: Some(BoardRecipient::for_actor(&actor)),
                supersedes: None,
                repo_key: None,
                state: None,
            },
        )?;
        tx.execute(
            "UPDATE entries SET worker_id=?2 WHERE id=?1",
            params![sql_number(entry.get()), worker_id],
        )
        .map_err(sql_error)?;
    }
    Ok(())
}

/// Bound workers of the task's live claim, read before that claim closes.
fn bound_workers_for_live_claim(
    tx: &Transaction<'_>,
    task: TaskId,
) -> Result<Vec<(i64, String, BoardActor)>, BoardError> {
    let mut statement = tx
        .prepare(concat!(
            "SELECT w.id,w.handle,a.user,a.host,a.harness,a.session ",
            "FROM claim_workers w JOIN claims c ON c.id=w.claim_id JOIN actors a ON a.id=w.actor_id ",
            "WHERE c.plan_id=?1 AND c.task_ordinal=?2 AND c.ended_at IS NULL AND w.ended_at IS NULL ",
            "ORDER BY w.id"
        ))
        .map_err(sql_error)?;
    let rows = statement
        .query_map(
            params![sql_number(task.plan.get()), sql_number(task.ordinal)],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    actor_from_row(row, 2)?,
                ))
            },
        )
        .map_err(sql_error)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql_error)
}

fn current_claim(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    task: TaskId,
) -> Result<super::StoredClaim, BoardError> {
    active_claim(tx, task, ctx.now, ctx.claim_ttl_secs)?.ok_or_else(|| {
        invalid(
            BoardErrorCode::InvalidState,
            format!("{task} has no current claim"),
        )
    })
}

fn worker_manager_context(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    task: TaskId,
    holder: &super::StoredClaim,
) -> Result<WriteContext, BoardError> {
    if !holder.record.stale && holder.actor_id == ctx.actor_id {
        return Ok(ctx.with_authority(AuthoritySource::Holder));
    }
    if !holder.record.stale
        && holder
            .record
            .delegated_by
            .as_ref()
            .is_some_and(|delegator| *delegator == ctx.actor)
    {
        return Ok(ctx.with_authority(AuthoritySource::Delegator));
    }
    let authority = super::super::super::board_authority::authority_for(
        tx,
        &ctx.actor,
        Some(task.plan),
        AuthorityRole::Assign,
        ctx.now,
    )?
    .ok_or_else(|| {
        invalid(
            BoardErrorCode::InvalidActor,
            "worker management requires the current holder, its delegator, or assign authority",
        )
    })?;
    Ok(ctx.with_authority(authority))
}

fn validate_worker_files(files: &[String]) -> Result<(), BoardError> {
    if files.is_empty() || files.len() > WORKER_FILE_LIMIT {
        return Err(invalid(
            BoardErrorCode::InvalidOptions,
            format!("worker requires 1..{WORKER_FILE_LIMIT} file paths"),
        ));
    }
    for (index, file) in files.iter().enumerate() {
        if file.trim().is_empty()
            || file.len() > WORKER_FILE_PATH_LIMIT
            || file.chars().any(char::is_control)
        {
            return Err(invalid(
                BoardErrorCode::InvalidOptions,
                format!(
                    "worker file paths must be nonblank, at most {WORKER_FILE_PATH_LIMIT} bytes, and contain no control characters"
                ),
            ));
        }
        if files[..index].contains(file) {
            return Err(invalid(
                BoardErrorCode::InvalidOptions,
                format!("worker file path `{file}` is repeated"),
            ));
        }
    }
    Ok(())
}
