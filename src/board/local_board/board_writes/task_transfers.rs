//! Atomic cross-plan task succession with immutable history links.

#[cfg(test)]
mod tests;

use super::super::{
    BoardError, EntryDraft, WriteContext, insert_entry, insert_event, invalid, require_plan,
    sql_error, sql_number, sqlite_id,
};
use crate::board::board_actor::BoardRecipient;
use crate::board::board_ids::{EntryId, PlanId, TaskId};
use crate::board::board_protocol::{
    AuthorityRole, AuthoritySource, BoardChange, BoardReply, BoardResult,
};
use crate::board::board_vocabulary::{EntryKind, EntryText, PlanTitle, TaskColumn};
use rusqlite::{OptionalExtension, Transaction, params};

pub(in crate::board::local_board) fn transfer_task(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    task: TaskId,
    destination: PlanId,
    note: &EntryText,
) -> Result<BoardReply, BoardError> {
    if task.plan == destination {
        return Err(invalid(
            "invalid_reference",
            "task transfer requires a different destination plan",
        ));
    }
    require_plan(tx, task.plan)?;
    require_plan(tx, destination)?;
    super::plan_lifecycle::require_open(tx, task.plan)?;
    super::plan_lifecycle::require_open(tx, destination)?;
    require_current(tx, task)?;
    if super::super::board_queries::board_reads::task_lineage::has_live_claims_or_workers(tx, task)?
    {
        return Err(invalid(
            "invalid_state",
            format!("{task} has a live claim or worker and cannot be transferred"),
        ));
    }

    let source: Option<(String, String, Option<String>, Option<String>)> = tx
        .query_row(
            "SELECT title,column_name,assignee,section FROM tasks WHERE plan_id=?1 AND ordinal=?2",
            params![sql_number(task.plan.get()), sql_number(task.ordinal)],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(sql_error)?;
    let (title, column, assignee, section) =
        source.ok_or_else(|| invalid("invalid_reference", format!("unknown task {task}")))?;
    let title = PlanTitle::new(title).map_err(BoardError::from)?;
    let column: TaskColumn = column.parse().map_err(BoardError::from)?;
    if column == TaskColumn::Done {
        return Err(invalid(
            "invalid_state",
            format!("{task} is done and cannot be transferred"),
        ));
    }
    let assignee = assignee
        .as_deref()
        .map(BoardRecipient::parse)
        .transpose()
        .map_err(BoardError::from)?;

    let source_decide = require_authority(tx, ctx, task.plan, AuthorityRole::Decide)?;
    let destination_decide = require_authority(tx, ctx, destination, AuthorityRole::Decide)?;
    let destination_assign = if assignee.is_some() {
        Some(require_authority(
            tx,
            ctx,
            destination,
            AuthorityRole::Assign,
        )?)
    } else {
        None
    };
    let authorized = ctx.with_authority(source_decide.clone());

    let destination_task = super::task_writes::allocate_task(
        tx,
        ctx,
        destination,
        &title,
        assignee.as_ref(),
        section.as_deref(),
    )?;
    tx.execute(
        "UPDATE tasks SET column_name=?3,seq=?4 WHERE plan_id=?1 AND ordinal=?2",
        params![
            sql_number(destination_task.plan.get()),
            sql_number(destination_task.ordinal),
            column.as_str(),
            sql_number(ctx.seq.get())
        ],
    )
    .map_err(sql_error)?;

    let entry = insert_entry(
        tx,
        &authorized,
        &EntryDraft {
            plan_id: Some(task.plan),
            kind: EntryKind::TaskTransfer,
            body: note.as_str().to_owned(),
            to_whom: assignee.clone(),
            supersedes: None,
            repo_key: None,
            state: None,
        },
    )?;
    for reference in [task, destination_task] {
        tx.execute(
            "INSERT OR IGNORE INTO entry_refs(entry_id,target) VALUES(?1,?2)",
            params![sql_number(entry.get()), reference.to_string()],
        )
        .map_err(sql_error)?;
    }
    let (source_basis, source_entry) = audit_source(&source_decide)?;
    let (destination_basis, destination_entry) = audit_source(&destination_decide)?;
    let (assign_basis, assign_entry) = destination_assign
        .as_ref()
        .map(audit_source)
        .transpose()?
        .map_or((None, None), |(basis, entry)| (Some(basis), entry));
    tx.execute(
        concat!(
            "INSERT INTO task_transfers(source_plan,source_ordinal,destination_plan,destination_ordinal,",
            "entry_id,source_decide_basis,source_decide_entry,destination_decide_basis,",
            "destination_decide_entry,destination_assign_basis,destination_assign_entry) ",
            "VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)"
        ),
        params![
            sql_number(task.plan.get()),
            sql_number(task.ordinal),
            sql_number(destination_task.plan.get()),
            sql_number(destination_task.ordinal),
            sql_number(entry.get()),
            source_basis,
            source_entry,
            destination_basis,
            destination_entry,
            assign_basis,
            assign_entry
        ],
    )
    .map_err(sql_error)?;
    tx.execute(
        "UPDATE tasks SET seq=?3 WHERE plan_id=?1 AND ordinal=?2",
        params![
            sql_number(task.plan.get()),
            sql_number(task.ordinal),
            sql_number(ctx.seq.get())
        ],
    )
    .map_err(sql_error)?;
    let summary = format!("transferred {task} to {destination_task}");
    insert_event(
        tx,
        &authorized,
        Some(task.plan),
        EntryKind::TaskTransfer,
        &task.to_string(),
        assignee.as_ref(),
        &summary,
    )?;
    Ok(change_reply(&authorized, entry, task, destination_task))
}

pub(in crate::board::local_board) fn require_current(
    conn: &rusqlite::Connection,
    task: TaskId,
) -> Result<(), BoardError> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE plan_id=?1 AND ordinal=?2)",
            params![sql_number(task.plan.get()), sql_number(task.ordinal)],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    if !exists {
        return Err(invalid("invalid_reference", format!("unknown task {task}")));
    }
    if let Some(successor) =
        super::super::board_queries::board_reads::task_lineage::links(conn, task)?.successor
    {
        return Err(invalid(
            "invalid_state",
            format!("{task} was transferred to {successor}; use the successor card"),
        ));
    }
    Ok(())
}

fn require_authority(
    conn: &rusqlite::Connection,
    ctx: &WriteContext,
    plan: PlanId,
    role: AuthorityRole,
) -> Result<AuthoritySource, BoardError> {
    crate::board::local_board::board_authority::authority_for(
        conn,
        &ctx.actor,
        Some(plan),
        role,
        ctx.now,
    )?
    .ok_or_else(|| {
        invalid(
            "invalid_actor",
            format!("{} lacks {role} authority for {plan}", ctx.actor),
        )
    })
}

fn audit_source(source: &AuthoritySource) -> Result<(&'static str, Option<i64>), BoardError> {
    match source.basis() {
        "owner" => Ok(("owner", None)),
        "grant" => Ok((
            "grant",
            source
                .grant_entry()
                .map(|entry| sqlite_id(entry.get()))
                .transpose()?,
        )),
        _ => Err(invalid(
            "board_unavailable",
            "task transfer resolved an unsupported authority source",
        )),
    }
}

fn change_reply(
    ctx: &WriteContext,
    entry: EntryId,
    source: TaskId,
    destination: TaskId,
) -> BoardReply {
    BoardReply::new(
        "local",
        BoardResult::Change(BoardChange {
            entry,
            seq: ctx.seq,
            plan: Some(source.plan),
            revision: None,
            task: Some(source),
            transferred_to: Some(destination),
            deduplicated: false,
        }),
    )
}
