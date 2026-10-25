//! Role-specific task card transitions.

use super::super::task_claims::{active_claim, claim_task, end_claim};
use super::*;
use crate::board::board_protocol::BoardErrorCode;
use crate::board::board_protocol::{AuthorityRole, AuthoritySource, ClaimEndReason, ClaimResume};
use crate::board::board_vocabulary::{EntryKind, EntryText, TaskColumn};

pub(in crate::board::local_board) fn move_task(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    task: TaskId,
    column: TaskColumn,
    to: Option<&BoardRecipient>,
) -> Result<BoardReply, BoardError> {
    let normalized_to = normalized_task_recipient(to, &ctx.actor)?;
    let to = normalized_to.as_ref();
    let card = require_task(tx, task)?;
    let holder = active_claim(tx, task, ctx.now, ctx.claim_ttl_secs)?;
    let holder_is_actor = holder
        .as_ref()
        .is_some_and(|claim| claim.actor_id == ctx.actor_id);
    let holder_is_other = holder.is_some() && !holder_is_actor;
    let stale_other = holder
        .as_ref()
        .is_some_and(|claim| claim.actor_id != ctx.actor_id && claim.record.stale);
    let release_column = matches!(column, TaskColumn::Todo | TaskColumn::Blocked);
    let creator_plain_move = to.is_none()
        && holder.is_none()
        && card.assignee.is_none()
        && matches!(card.column, TaskColumn::Todo | TaskColumn::Blocked)
        && release_column
        && card.column != column
        && task_created_by(tx, task, ctx.actor_id)?;
    if stale_other && !release_column {
        return Err(invalid(
            BoardErrorCode::InvalidActor,
            format!("stale claim on {task} may only be released to todo or blocked"),
        ));
    }

    let decision_authority = if card.column == TaskColumn::Done && column == TaskColumn::Todo {
        super::super::super::board_authority::authority_for(
            tx,
            &ctx.actor,
            Some(task.plan),
            AuthorityRole::Decide,
            ctx.now,
        )?
    } else {
        None
    };
    if card.column == TaskColumn::Done && column != TaskColumn::Done {
        if column != TaskColumn::Todo || decision_authority.is_none() {
            return Err(invalid(
                BoardErrorCode::InvalidState,
                format!("{task} is done and can only be reopened to todo with decide authority"),
            ));
        }
    }

    let assigned_to_actor = card
        .assignee
        .as_ref()
        .is_some_and(|assignee| assignee.matches(&ctx.actor));
    let cancel_pending_assignment = to.is_none()
        && column == TaskColumn::Todo
        && card.column == TaskColumn::Doing
        && holder.is_none()
        && card.assignee.is_some()
        && !assigned_to_actor;
    let assign_for_plain_release = to.is_none()
        && holder.is_none()
        && !assigned_to_actor
        && release_column
        && !creator_plain_move;
    let assign_authority = if to.is_some() || cancel_pending_assignment || assign_for_plain_release
    {
        super::super::super::board_authority::authority_for(
            tx,
            &ctx.actor,
            Some(task.plan),
            AuthorityRole::Assign,
            ctx.now,
        )?
    } else {
        None
    };
    if (to.is_some() || cancel_pending_assignment) && assign_authority.is_none() {
        return Err(invalid(
            BoardErrorCode::InvalidActor,
            if to.is_some() {
                "task assignment requires assign authority"
            } else {
                "cancelling another actor's pending assignment requires assign authority"
            },
        ));
    }
    if matches!(column, TaskColumn::Done | TaskColumn::Review) && !holder_is_actor {
        return Err(invalid(
            BoardErrorCode::InvalidActor,
            format!("only the active claim holder may move {task} to {column}"),
        ));
    }

    let released_by_delegator = holder.as_ref().is_some_and(|claim| {
        claim
            .record
            .delegated_by
            .as_ref()
            .is_some_and(|delegator| *delegator == ctx.actor)
    });
    let release_authority =
        if holder_is_other && release_column && !released_by_delegator && !stale_other {
            super::super::super::board_authority::authority_for(
                tx,
                &ctx.actor,
                Some(task.plan),
                AuthorityRole::Release,
                ctx.now,
            )?
        } else {
            None
        };
    let has_release_authority = release_authority.is_some();
    let delegated_release = holder_is_other && release_column && released_by_delegator;
    let bound_worker = is_bound_worker(tx, task, ctx.actor_id)?;
    let reopening_done = card.column == TaskColumn::Done && column == TaskColumn::Todo;
    let task_authority = if to.is_some() {
        assign_authority
    } else if reopening_done {
        decision_authority
    } else if delegated_release {
        Some(AuthoritySource::Delegator)
    } else if stale_other {
        Some(AuthoritySource::Stale)
    } else if release_authority.is_some() {
        release_authority
    } else if holder_is_actor {
        Some(AuthoritySource::Holder)
    } else if creator_plain_move {
        Some(AuthoritySource::Creator)
    } else {
        assign_authority
    };
    if bound_worker && !holder_is_actor && !assigned_to_actor && task_authority.is_none() {
        return Err(invalid(
            BoardErrorCode::InvalidActor,
            "a bound worker needs separate claim, assignee, or grant authority to move the lead task",
        ));
    }
    if column == TaskColumn::Doing && to.is_none() && !holder_is_actor {
        if stale_other {
            return Err(invalid(
                BoardErrorCode::InvalidActor,
                format!("stale claim on {task} may only be released to todo or blocked"),
            ));
        }
        let scope: Option<String> = tx.query_row(
            "SELECT scope FROM claims WHERE plan_id=?1 AND task_ordinal=?2 ORDER BY id DESC LIMIT 1",
            params![sql_number(task.plan.get()), sql_number(task.ordinal)],
            |row| row.get(0),
        ).optional().map_err(sql_error)?;
        let scope = EntryText::new(scope.unwrap_or_else(|| card.title.as_str().to_owned()))
            .map_err(BoardError::from)?;
        return claim_task(tx, ctx, task, Some(&scope), ClaimResume::No, None);
    }

    if holder.is_none() && !assigned_to_actor && task_authority.is_none() {
        require_assignee(&card, &ctx.actor, task)?;
        return Err(invalid(
            BoardErrorCode::InvalidActor,
            format!(
                "{task} requires its assignee or assign authority to move; use --to to assign a recipient"
            ),
        ));
    }

    let assign = to.is_some();
    if holder_is_other && !stale_other && !delegated_release && !has_release_authority && !assign {
        return Err(invalid(
            BoardErrorCode::InvalidActor,
            format!("cannot move {task} held by another actor"),
        ));
    }
    let recipient = to.cloned().or_else(|| {
        holder
            .as_ref()
            .map(|claim| BoardRecipient::for_actor(&claim.record.actor))
    });
    if assign || column != TaskColumn::Doing {
        end_claim(
            tx,
            ctx,
            task,
            if assign {
                ClaimEndReason::Reassigned
            } else {
                ClaimEndReason::Released
            },
        )?;
    }
    tx.execute(
        "UPDATE tasks SET column_name=?3,assignee=coalesce(?4,assignee),seq=?5 WHERE plan_id=?1 AND ordinal=?2",
        params![
            sql_number(task.plan.get()),
            sql_number(task.ordinal),
            column.as_str(),
            to.map(BoardRecipient::as_str),
            sql_number(ctx.seq.get())
        ],
    ).map_err(sql_error)?;
    let mut summary = format!("{task} -> {column}");
    if let Some(to) = to {
        summary.push_str(&format!(" (to {to})"));
    }
    if assign {
        summary.push_str("; assigned, awaiting recipient claim");
        if let Some(holder) = holder {
            summary.push_str(&format!("; reassigned from {}", holder.record.actor));
        }
    }
    let action_ctx =
        task_authority.map_or_else(|| ctx.clone(), |source| ctx.with_authority(source));
    let entry = task_entry(tx, &action_ctx, task, summary.clone(), recipient.as_ref())?;
    insert_event(
        tx,
        &action_ctx,
        Some(task.plan),
        EntryKind::Task,
        &task.to_string(),
        recipient.as_ref(),
        &summary,
    )?;
    Ok(action_ctx.change_reply(entry, Some(task.plan), None, Some(task)))
}

/// Task rows do not store their creator, so read it from the immutable create event.
fn task_created_by(tx: &Transaction<'_>, task: TaskId, actor_id: i64) -> Result<bool, BoardError> {
    let created_by: Option<i64> = tx
        .query_row(
            concat!(
                "SELECT e.actor_id FROM entries e JOIN entry_refs r ON r.entry_id=e.id ",
                "WHERE r.target=?1 AND e.kind='task' AND e.body LIKE ?2 ",
                "ORDER BY e.seq ASC LIMIT 1"
            ),
            params![task.to_string(), format!("created {task}:%")],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    Ok(created_by == Some(actor_id))
}
