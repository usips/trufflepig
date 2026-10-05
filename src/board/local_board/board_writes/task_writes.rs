//! Task creation, assignment reservations, and card column transitions.

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::task_claims::{active_claim, claim_task, end_claim};
use super::super::{
    BoardError, EntryDraft, WriteContext, can_accept, insert_entry, insert_event, invalid,
    require_plan, row_number, sql_error, sql_number,
};
use crate::board::board_actor::{BoardActor, BoardRecipient};
use crate::board::board_ids::{EntryId, PlanId, TaskId};
use crate::board::board_protocol::{BoardReply, ClaimEndReason, ClaimResume};
use crate::board::board_vocabulary::{EntryKind, EntryText, PlanTitle, TaskColumn};

pub(in crate::board::local_board) struct TaskCard {
    title: PlanTitle,
    pub(in crate::board::local_board) column: TaskColumn,
    assignee: Option<BoardRecipient>,
}

pub(in crate::board::local_board) fn require_task(conn: &Connection, task: TaskId) -> Result<TaskCard, BoardError> {
    let stored: Option<(String, String, Option<String>)> = conn
        .query_row(
            "SELECT title,column_name,assignee FROM tasks WHERE plan_id=?1 AND ordinal=?2",
            params![sql_number(task.plan.get()), sql_number(task.ordinal)],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(sql_error)?;
    let (title, column, assignee) =
        stored.ok_or_else(|| invalid("invalid_reference", format!("unknown task {task}")))?;
    Ok(TaskCard {
        title: PlanTitle::new(title).map_err(BoardError::from)?,
        column: column.parse().map_err(BoardError::from)?,
        assignee: assignee
            .as_deref()
            .map(BoardRecipient::parse)
            .transpose()
            .map_err(BoardError::from)?,
    })
}

pub(in crate::board::local_board) fn require_assignee(
    card: &TaskCard,
    actor: &BoardActor,
    task: TaskId,
) -> Result<(), BoardError> {
    if card.column == TaskColumn::Doing
        && card
            .assignee
            .as_ref()
            .is_some_and(|assignee| !assignee.matches(actor))
    {
        return Err(invalid(
            "invalid_actor",
            format!("{task} is reserved for its assignee"),
        ));
    }
    Ok(())
}

pub(in crate::board::local_board) fn create_task(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    plan: PlanId,
    title: &PlanTitle,
    to: Option<&BoardRecipient>,
    section: Option<&str>,
) -> Result<BoardReply, BoardError> {
    require_plan(tx, plan)?;
    let task = allocate_task(tx, ctx, plan, title, to, section)?;
    let summary = format!("created {task}: {}", title.as_str());
    let entry = task_entry(tx, ctx, task, summary.clone(), to)?;
    insert_event(
        tx,
        ctx,
        Some(plan),
        EntryKind::Task,
        &task.to_string(),
        to,
        &summary,
    )?;
    Ok(ctx.change_reply(entry, Some(task.plan), None, Some(task)))
}

pub(in crate::board::local_board) fn allocate_task(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    plan: PlanId,
    title: &PlanTitle,
    to: Option<&BoardRecipient>,
    section: Option<&str>,
) -> Result<TaskId, BoardError> {
    let ordinal: u64 = tx
        .query_row(
            "UPDATE plans SET next_task=next_task+1 WHERE id=?1 RETURNING next_task",
            [sql_number(plan.get())],
            |row| row_number(row, 0),
        )
        .map_err(sql_error)?;
    let task = TaskId::new(plan, ordinal).map_err(BoardError::from)?;
    tx.execute(
        "INSERT INTO tasks(plan_id,ordinal,title,column_name,assignee,section,seq) VALUES(?1,?2,?3,'todo',?4,?5,?6)",
        params![
            sql_number(plan.get()),
            sql_number(ordinal),
            title.as_str(),
            to.map(BoardRecipient::as_str),
            section,
            sql_number(ctx.seq.get())
        ],
    ).map_err(sql_error)?;
    Ok(task)
}

pub(in crate::board::local_board) fn move_task(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    task: TaskId,
    column: TaskColumn,
    to: Option<&BoardRecipient>,
) -> Result<BoardReply, BoardError> {
    let card = require_task(tx, task)?;
    let privileged = can_accept(tx, &ctx.actor, task.plan)?;
    if card.column == TaskColumn::Done
        && column != TaskColumn::Done
        && !(column == TaskColumn::Todo && privileged)
    {
        return Err(invalid(
            "invalid_state",
            format!("{task} is done and cannot be reopened"),
        ));
    }
    let holder = active_claim(tx, task, ctx.now, ctx.claim_ttl_secs)?;
    if holder.is_none() && !privileged {
        require_assignee(&card, &ctx.actor, task)?;
    }
    let reassign = column == TaskColumn::Doing && to.is_some();
    if reassign && !privileged {
        return Err(invalid(
            "invalid_actor",
            "task reassignment requires the plan owner's human or steward identity",
        ));
    }
    if column == TaskColumn::Doing
        && to.is_none()
        && holder
            .as_ref()
            .is_none_or(|claim| claim.actor_id != ctx.actor_id)
    {
        let scope: Option<String> = tx.query_row(
            "SELECT scope FROM claims WHERE plan_id=?1 AND task_ordinal=?2 ORDER BY id DESC LIMIT 1",
            params![sql_number(task.plan.get()), sql_number(task.ordinal)], |row| row.get(0),
        ).optional().map_err(sql_error)?;
        let scope = EntryText::new(scope.unwrap_or_else(|| card.title.as_str().to_owned()))
            .map_err(BoardError::from)?;
        return claim_task(tx, ctx, task, Some(&scope), ClaimResume::No, None);
    }
    // A delegator may release the lease it delegated; reassignment above
    // still requires plan privilege.
    let held_by_other = holder
        .as_ref()
        .is_some_and(|claim| claim.actor_id != ctx.actor_id);
    let released_by_delegator = holder.as_ref().is_some_and(|claim| {
        claim
            .record
            .delegated_by
            .as_ref()
            .is_some_and(|delegator| *delegator == ctx.actor)
    });
    if held_by_other && !privileged && !released_by_delegator {
        return Err(invalid(
            "invalid_actor",
            format!("cannot move {task} held by another actor"),
        ));
    }
    let recipient = to.cloned().or_else(|| {
        holder
            .as_ref()
            .map(|claim| BoardRecipient::for_actor(&claim.record.actor))
    });
    if reassign || column != TaskColumn::Doing {
        end_claim(
            tx,
            task,
            ctx.now,
            if reassign {
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
    if reassign {
        summary.push_str("; assigned, awaiting recipient claim");
        if let Some(holder) = holder {
            summary.push_str(&format!("; reassigned from {}", holder.record.actor));
        }
    }
    let entry = task_entry(tx, ctx, task, summary.clone(), recipient.as_ref())?;
    insert_event(
        tx,
        ctx,
        Some(task.plan),
        EntryKind::Task,
        &task.to_string(),
        recipient.as_ref(),
        &summary,
    )?;
    Ok(ctx.change_reply(entry, Some(task.plan), None, Some(task)))
}

fn task_entry(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    task: TaskId,
    body: String,
    to: Option<&BoardRecipient>,
) -> Result<EntryId, BoardError> {
    insert_entry(
        tx,
        ctx,
        &EntryDraft {
            plan_id: Some(task.plan),
            kind: EntryKind::Task,
            body,
            to_whom: to.cloned(),
            supersedes: None,
            repo_key: None,
            state: None,
        },
    )
}
