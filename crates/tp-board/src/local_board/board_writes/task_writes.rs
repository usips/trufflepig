//! Task creation, assignment reservations, and card column transitions.

mod task_move_authority;
mod task_moves;

pub(in crate::board::local_board) use task_move_authority::is_bound_worker;
pub(in crate::board::local_board) use task_moves::move_task;

use crate::board::board_protocol::BoardErrorCode;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::super::{
    BoardError, EntryDraft, WriteContext, insert_entry, insert_event, invalid, require_plan,
    row_number, sql_error, sql_number,
};
use crate::board::board_actor::{BoardActor, BoardRecipient};
use crate::board::board_ids::{EntryId, PlanId, TaskId};
use crate::board::board_protocol::{AuthorityRole, BoardReply};
use crate::board::board_vocabulary::{EntryKind, PlanTitle, TaskColumn};

#[cfg(test)]
mod tests;

pub(in crate::board::local_board) struct TaskCard {
    title: PlanTitle,
    pub(in crate::board::local_board) column: TaskColumn,
    assignee: Option<BoardRecipient>,
}

pub(in crate::board::local_board) fn require_task(
    conn: &Connection,
    task: TaskId,
) -> Result<TaskCard, BoardError> {
    let stored: Option<(String, String, Option<String>)> = conn
        .query_row(
            "SELECT title,column_name,assignee FROM tasks WHERE plan_id=?1 AND ordinal=?2",
            params![sql_number(task.plan.get()), sql_number(task.ordinal)],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(sql_error)?;
    let (title, column, assignee) = stored.ok_or_else(|| {
        invalid(
            BoardErrorCode::InvalidReference,
            format!("unknown task {task}"),
        )
    })?;
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
            BoardErrorCode::InvalidActor,
            format!("{task} is reserved for its assignee"),
        ));
    }
    Ok(())
}

/// Expands a task recipient for the caller and refuses task addresses, which no delivery matches.
pub(super) fn normalized_task_recipient(
    to: Option<&BoardRecipient>,
    caller: &BoardActor,
) -> Result<Option<BoardRecipient>, BoardError> {
    let Some(recipient) = to.map(|recipient| recipient.normalize_for_caller(caller)) else {
        return Ok(None);
    };
    if TaskId::parse(recipient.as_str()).is_ok() {
        return Err(invalid(
            BoardErrorCode::InvalidOptions,
            "a task cannot be assigned to another task",
        ));
    }
    Ok(Some(recipient))
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
    let task_recipient = normalized_task_recipient(to, &ctx.actor)?;
    let self_assigned = task_recipient
        .as_ref()
        .is_some_and(|recipient| recipient == &BoardRecipient::for_actor(&ctx.actor));
    let task_ctx = if task_recipient.is_some() && !self_assigned {
        let authority = super::super::board_authority::authority_for(
            tx,
            &ctx.actor,
            Some(plan),
            AuthorityRole::Assign,
            ctx.now,
        )?
        .ok_or_else(|| {
            invalid(
                BoardErrorCode::InvalidActor,
                format!("assigning a recipient to a new task in {plan} requires assign authority"),
            )
        })?;
        ctx.with_authority(authority)
    } else {
        ctx.clone()
    };
    let task = allocate_task(tx, ctx, plan, title, task_recipient.as_ref(), section)?;
    let summary = format!("created {task}: {}", title.as_str());
    let entry = task_entry(
        tx,
        &task_ctx,
        task,
        summary.clone(),
        task_recipient.as_ref(),
    )?;
    insert_event(
        tx,
        &task_ctx,
        Some(plan),
        EntryKind::Task,
        &task.to_string(),
        task_recipient.as_ref(),
        &summary,
    )?;
    Ok(task_ctx.change_reply(entry, Some(task.plan), None, Some(task)))
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

pub(super) fn task_entry(
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
