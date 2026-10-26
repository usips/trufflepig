//! Exclusive task leases, ordinary activity, and durable claim history.

#[cfg(test)]
mod tests;

mod claim_activity;
mod claim_history;
mod claim_holders;
mod session_lifecycle;
mod worker_lifecycle;

use super::super::board_queries::worker_reads::{
    active_bound_worker_for_actor, active_worker_binding, worker_for_address,
};
use crate::board_protocol::BoardErrorCode;
pub(in crate::local_board) use claim_activity::{
    refresh_commit_claims, refresh_inbox_claims, refresh_plan_claims,
};
pub(in crate::local_board) use claim_history::{active_claim, read_claims_window, read_tasks};
use claim_holders::{claim_conflict, resolve_delegate};
pub(in crate::local_board) use session_lifecycle::{bye, settled_bye};
pub(in crate::local_board) use worker_lifecycle::{
    add_worker, attach_worker_post, bind_worker, end_worker, end_workers_for_claim,
};

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use super::super::{
    BoardError, EntryDraft, WriteContext, actor_from_row, delegated_actor_from_row,
    ensure_actor_without_seen_bump, insert_entry, insert_event, invalid, require_plan, row_number,
    sql_error, sql_number,
};
use super::task_writes::{allocate_task, require_assignee, require_task};
#[cfg(test)]
use super::task_writes::{create_task, move_task};
use crate::board_actor::{BoardActor, BoardRecipient, claim_vendor};
use crate::board_ids::{EntryId, EventSeq, PlanId, TaskId};
use crate::board_protocol::{
    BoardReply, ClaimDelegate, ClaimEndReason, ClaimRecord, ClaimResume, CommitCoauthor, TaskRecord,
};
use crate::board_vocabulary::{EntryKind, EntryText, PlanTitle, TaskColumn};

/// Idle time required before a bare `--resume` may replace a live lease.
pub(in crate::local_board) const RESUME_IDLE_GRACE_SECS: i64 = 600;

pub(in crate::local_board) struct StoredClaim {
    pub(in crate::local_board) actor_id: i64,
    pub(in crate::local_board) record: ClaimRecord,
}

pub(in crate::local_board) fn carve_claim(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    plan: PlanId,
    title: &PlanTitle,
    scope: &EntryText,
    section: Option<&str>,
) -> Result<BoardReply, BoardError> {
    require_plan(tx, plan)?;
    let task = allocate_task(tx, ctx, plan, title, None, section)?;
    claim_task(tx, ctx, task, Some(scope), ClaimResume::No, None)
}

pub(in crate::local_board) fn claim_task(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    task: TaskId,
    scope: Option<&EntryText>,
    resume: ClaimResume,
    delegate: Option<&ClaimDelegate>,
) -> Result<BoardReply, BoardError> {
    let card = require_task(tx, task)?;
    if card.column == TaskColumn::Done {
        return Err(invalid(
            BoardErrorCode::InvalidState,
            format!("{task} is done and cannot be claimed"),
        ));
    }
    if super::task_writes::is_bound_worker(tx, task, ctx.actor_id)? {
        let holder = active_claim(tx, task, ctx.now, ctx.claim_ttl_secs)?;
        let already_lead = holder
            .as_ref()
            .is_some_and(|claim| claim.actor_id == ctx.actor_id);
        if !already_lead {
            let assignee: Option<String> = tx
                .query_row(
                    "SELECT assignee FROM tasks WHERE plan_id=?1 AND ordinal=?2",
                    params![sql_number(task.plan.get()), sql_number(task.ordinal)],
                    |row| row.get(0),
                )
                .map_err(sql_error)?;
            let assigned_to_actor = assignee
                .as_deref()
                .map(BoardRecipient::parse)
                .transpose()
                .map_err(BoardError::from)?
                .is_some_and(|recipient| recipient.matches(&ctx.actor));
            let assign_authority = super::super::board_authority::authority_for(
                tx,
                &ctx.actor,
                Some(task.plan),
                crate::board_protocol::AuthorityRole::Assign,
                ctx.now,
            )?;
            if !assigned_to_actor && assign_authority.is_none() {
                return Err(invalid(
                    BoardErrorCode::InvalidActor,
                    "a bound worker needs separate claim, assignee, or assign authority to claim the lead task",
                ));
            }
        }
    }
    let delegation = match delegate {
        Some(target) => Some(resolve_delegate(tx, ctx, task, target)?),
        None => None,
    };
    let held;
    let delegator = ctx;
    let (ctx, delegated_by) = match &delegation {
        Some(resolved) => {
            held = resolved.holder_context(ctx);
            (&held, Some(resolved.delegated_by))
        }
        None => (ctx, None),
    };
    let holder = active_claim(tx, task, ctx.now, ctx.claim_ttl_secs)?;
    if holder.is_none() {
        require_assignee(&card, &ctx.actor, task)?;
    }
    if resume.is_resuming() {
        let previous = holder.as_ref().ok_or_else(|| {
            invalid(
                BoardErrorCode::InvalidState,
                format!("{task} has no claim to resume"),
            )
        })?;
        if let ClaimResume::Entry(entry) = resume {
            if previous.record.entry != entry {
                return Err(invalid(
                    BoardErrorCode::InvalidReference,
                    format!("{entry} is not the current claim for {task}"),
                ));
            }
        }
        if previous.record.actor.user != ctx.actor.user
            || previous.record.actor.host != ctx.actor.host
            || previous.record.actor.harness != ctx.actor.harness
        {
            return Err(invalid(
                BoardErrorCode::InvalidActor,
                "resume requires the same user, host, and harness",
            ));
        }
    }
    if delegation.is_none() && (resume.is_resuming() || scope.is_some()) {
        if let Some(holder) = holder
            .as_ref()
            .filter(|claim| claim.actor_id == ctx.actor_id)
        {
            // The caller already holds the lease: refresh it in place, keeping
            // the claim row, entry, claimed_at, and bound workers. No event is
            // written, so the receipt points at the claim entry's own seq.
            let entry = holder.record.entry;
            if let Some(text) = scope {
                tx.execute(
                    "UPDATE claims SET scope=?4,last_active=?5 WHERE plan_id=?1 AND task_ordinal=?2 AND entry_id=?3 AND ended_at IS NULL",
                    params![
                        sql_number(task.plan.get()),
                        sql_number(task.ordinal),
                        sql_number(entry.get()),
                        text.as_str(),
                        ctx.now
                    ],
                )
                .map_err(sql_error)?;
            } else {
                tx.execute(
                    "UPDATE claims SET last_active=?4 WHERE plan_id=?1 AND task_ordinal=?2 AND entry_id=?3 AND ended_at IS NULL",
                    params![
                        sql_number(task.plan.get()),
                        sql_number(task.ordinal),
                        sql_number(entry.get()),
                        ctx.now
                    ],
                )
                .map_err(sql_error)?;
            }
            let seq = EventSeq::new(
                tx.query_row(
                    "SELECT seq FROM entries WHERE id=?1",
                    [sql_number(entry.get())],
                    |row| row_number(row, 0),
                )
                .map_err(sql_error)?,
            );
            return Ok(ctx.change_reply_at(seq, entry, Some(task.plan), Some(task)));
        }
    }
    let scope = scope
        .or_else(|| {
            holder
                .as_ref()
                .filter(|_| resume.is_resuming())
                .map(|claim| &claim.record.scope)
        })
        .ok_or_else(|| {
            invalid(
                BoardErrorCode::InvalidOptions,
                "claiming a task requires scope or resume",
            )
        })?;
    if let Some(holder) = &holder {
        match resume {
            ClaimResume::No if holder.actor_id != ctx.actor_id && !holder.record.stale => {
                return Err(claim_conflict(task, &holder.record, ctx.now));
            }
            ClaimResume::Idle
                if holder.actor_id != ctx.actor_id
                    && holder.record.last_active
                        >= ctx.now.saturating_sub(RESUME_IDLE_GRACE_SECS) =>
            {
                return Err(claim_conflict(task, &holder.record, ctx.now));
            }
            _ => {}
        }
        let reason = if resume.is_resuming() {
            ClaimEndReason::Resumed
        } else if holder.actor_id == ctx.actor_id {
            ClaimEndReason::Released
        } else {
            ClaimEndReason::TakenOver
        };
        let _ = end_claim(tx, ctx, task, reason)?;
    }
    let entry = insert_entry(
        tx,
        ctx,
        &EntryDraft {
            plan_id: Some(task.plan),
            kind: EntryKind::Claim,
            body: scope.as_str().to_owned(),
            to_whom: None,
            supersedes: None,
            repo_key: None,
            state: None,
        },
    )?;
    tx.execute(
        concat!(
            "INSERT INTO claims(plan_id,task_ordinal,actor_id,entry_id,scope,claimed_at,last_active,delegated_by) ",
            "VALUES(?1,?2,?3,?4,?5,?6,?6,?7)"
        ),
        params![
            sql_number(task.plan.get()),
            sql_number(task.ordinal),
            ctx.actor_id,
            sql_number(entry.get()),
            scope.as_str(),
            ctx.now,
            delegated_by
        ],
    ).map_err(sql_error)?;
    if delegation.is_some() {
        // A delegated lease makes its holder live again, so the holder's later bye releases it.
        tx.execute(
            "UPDATE agent_sessions SET ended_at=NULL WHERE actor_id=?1",
            [ctx.actor_id],
        )
        .map_err(sql_error)?;
    }
    let assignee = BoardRecipient::for_actor(&ctx.actor);
    tx.execute(
        "UPDATE tasks SET column_name='doing',assignee=?3,seq=?4 WHERE plan_id=?1 AND ordinal=?2",
        params![
            sql_number(task.plan.get()),
            sql_number(task.ordinal),
            assignee.as_str(),
            sql_number(ctx.seq.get())
        ],
    )
    .map_err(sql_error)?;
    let previous = holder
        .as_ref()
        .filter(|claim| claim.actor_id != ctx.actor_id);
    // A takeover notifies the previous holder, even when a delegation takes
    // over their stale lease. A fresh delegated claim notifies the delegate:
    // the event is authored by the delegator and addressed to the delegate,
    // so inbox filtering keeps it visible instead of hiding it as an own event.
    let recipient = previous
        .as_ref()
        .map(|claim| BoardRecipient::for_actor(&claim.record.actor))
        .or_else(|| {
            delegation
                .as_ref()
                .map(|resolved| BoardRecipient::for_actor(&resolved.actor))
        });
    let preview_end = scope
        .as_str()
        .char_indices()
        .map(|(offset, _)| offset)
        .take_while(|offset| *offset <= 2048)
        .last()
        .unwrap_or(0);
    let preview = if scope.as_str().len() > 2048 {
        &scope.as_str()[..preview_end]
    } else {
        scope.as_str()
    };
    let summary = match previous {
        Some(claim) if resume.is_resuming() => format!(
            "took {task}: {preview}; resumed claim from {}",
            claim.record.actor
        ),
        Some(claim) => format!(
            "took {task}: {preview}; took over stale claim from {}",
            claim.record.actor
        ),
        None => format!("took {task}: {preview}"),
    };
    let summary = match &delegation {
        Some(_) => format!("{summary} (via {})", delegator.actor.identity()),
        None => summary,
    };
    let event_ctx = match &delegation {
        Some(_) => delegator,
        None => ctx,
    };
    insert_event(
        tx,
        event_ctx,
        Some(task.plan),
        EntryKind::Claim,
        &entry.to_string(),
        recipient.as_ref(),
        &summary,
    )?;
    Ok(ctx.change_reply(entry, Some(task.plan), None, Some(task)))
}

pub(in crate::local_board) fn end_claim(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    task: TaskId,
    reason: ClaimEndReason,
) -> Result<Option<EntryId>, BoardError> {
    let claim_start = tx
        .query_row(
            "SELECT entry_id FROM claims WHERE plan_id=?1 AND task_ordinal=?2 AND ended_at IS NULL ORDER BY id DESC LIMIT 1",
            params![sql_number(task.plan.get()), sql_number(task.ordinal)],
            |row| row_number(row, 0),
        )
        .optional()
        .map_err(sql_error)?
        .map(EntryId::new)
        .transpose()
        .map_err(BoardError::from)?;
    end_workers_for_claim(tx, ctx, task)?;
    let ended = tx.execute(
        "UPDATE claims SET ended_at=?3,end_reason=?4 WHERE plan_id=?1 AND task_ordinal=?2 AND ended_at IS NULL",
        params![sql_number(task.plan.get()), sql_number(task.ordinal), ctx.now, reason.as_str()],
    ).map_err(sql_error)?;
    if ended == 0 {
        return Ok(None);
    }
    claim_start.map(Some).ok_or_else(|| {
        invalid(
            BoardErrorCode::BoardUnavailable,
            "ended claim has no start entry",
        )
    })
}
