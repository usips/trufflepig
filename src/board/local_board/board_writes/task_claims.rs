//! Exclusive task leases, ordinary activity, and durable claim history.

#[cfg(test)]
mod tests;

mod claim_activity;
mod claim_history;

pub(in crate::board::local_board) use claim_activity::{
    refresh_commit_claims, refresh_inbox_claims, refresh_plan_claims,
};
pub(in crate::board::local_board) use claim_history::{active_claim, read_claims_window, read_tasks};

use rusqlite::{Connection, Row, Transaction, params};

use super::task_writes::{allocate_task, require_assignee, require_task};
#[cfg(test)]
use super::task_writes::{create_task, move_task};
use super::super::{
    BoardError, EntryDraft, WriteContext, actor_from_row, insert_entry, insert_event, invalid,
    require_plan, row_number, sql_error, sql_number,
};
use crate::board::board_actor::{BoardRecipient, claim_vendor};
use crate::board::board_ids::{EntryId, EventSeq, PlanId, TaskId};
use crate::board::board_protocol::{
    BoardReply, ClaimEndReason, ClaimRecord, ClaimResume, CommitCoauthor, TaskRecord,
};
use crate::board::board_vocabulary::{EntryKind, EntryText, PlanTitle, TaskColumn};

/// Idle time required before a bare `--resume` may replace a live lease.
pub(in crate::board::local_board) const RESUME_IDLE_GRACE_SECS: i64 = 600;

pub(in crate::board::local_board) struct StoredClaim {
    pub(in crate::board::local_board) actor_id: i64,
    pub(in crate::board::local_board) record: ClaimRecord,
}

pub(in crate::board::local_board) fn carve_claim(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    plan: PlanId,
    title: &PlanTitle,
    scope: &EntryText,
    section: Option<&str>,
) -> Result<BoardReply, BoardError> {
    require_plan(tx, plan)?;
    let task = allocate_task(tx, ctx, plan, title, None, section)?;
    claim_task(tx, ctx, task, Some(scope), ClaimResume::No)
}

fn claim_conflict(task: TaskId, claim: &ClaimRecord, now: i64) -> BoardError {
    invalid(
        "claim_conflict",
        format!(
            "{task} held by {} ({}/{}) since {}, active {}s ago (last activity {})",
            claim.actor,
            claim.model.as_deref().unwrap_or("unknown"),
            claim.effort.as_deref().unwrap_or("unknown"),
            claim.claimed_at,
            now.saturating_sub(claim.last_active),
            claim.last_active,
        ),
    )
}

pub(in crate::board::local_board) fn claim_task(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    task: TaskId,
    scope: Option<&EntryText>,
    resume: ClaimResume,
) -> Result<BoardReply, BoardError> {
    let card = require_task(tx, task)?;
    if card.column == TaskColumn::Done {
        return Err(invalid(
            "invalid_state",
            format!("{task} is done and cannot be claimed"),
        ));
    }
    let holder = active_claim(tx, task, ctx.now, ctx.claim_ttl_secs)?;
    if holder.is_none() {
        require_assignee(&card, &ctx.actor, task)?;
    }
    if resume.is_resuming() {
        let previous = holder
            .as_ref()
            .ok_or_else(|| invalid("invalid_state", format!("{task} has no claim to resume")))?;
        if let ClaimResume::Entry(entry) = resume {
            if previous.record.entry != entry {
                return Err(invalid(
                    "invalid_reference",
                    format!("{entry} is not the current claim for {task}"),
                ));
            }
        }
        if previous.record.actor.user != ctx.actor.user
            || previous.record.actor.host != ctx.actor.host
            || previous.record.actor.harness != ctx.actor.harness
        {
            return Err(invalid(
                "invalid_actor",
                "resume requires the same user, host, and harness",
            ));
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
                "invalid_options",
                "claiming a task requires scope or resume",
            )
        })?;
    if let Some(holder) = &holder {
        match resume {
            ClaimResume::No if holder.actor_id != ctx.actor_id && !holder.record.stale => {
                return Err(claim_conflict(task, &holder.record, ctx.now));
            }
            ClaimResume::Idle
                if holder.record.last_active >= ctx.now.saturating_sub(RESUME_IDLE_GRACE_SECS) =>
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
        end_claim(tx, task, ctx.now, reason)?;
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
            "INSERT INTO claims(plan_id,task_ordinal,actor_id,entry_id,scope,claimed_at,last_active) ",
            "VALUES(?1,?2,?3,?4,?5,?6,?6)"
        ),
        params![
            sql_number(task.plan.get()),
            sql_number(task.ordinal),
            ctx.actor_id,
            sql_number(entry.get()),
            scope.as_str(),
            ctx.now
        ],
    ).map_err(sql_error)?;
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
    let recipient = previous
        .as_ref()
        .map(|claim| BoardRecipient::for_actor(&claim.record.actor));
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
    insert_event(
        tx,
        ctx,
        Some(task.plan),
        EntryKind::Claim,
        &entry.to_string(),
        recipient.as_ref(),
        &summary,
    )?;
    Ok(ctx.change_reply(entry, Some(task.plan), None, Some(task)))
}

pub(in crate::board::local_board) fn end_claim(
    tx: &Transaction<'_>,
    task: TaskId,
    now: i64,
    reason: ClaimEndReason,
) -> Result<(), BoardError> {
    tx.execute(
        "UPDATE claims SET ended_at=?3,end_reason=?4 WHERE plan_id=?1 AND task_ordinal=?2 AND ended_at IS NULL",
        params![sql_number(task.plan.get()), sql_number(task.ordinal), now, reason.as_str()],
    ).map_err(sql_error)?;
    Ok(())
}
