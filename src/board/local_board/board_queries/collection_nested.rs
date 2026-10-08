//! Task ordinal ceilings and claim source sequences bound current-state collection pages.

mod claim_windows;
mod overview_task_windows;
mod recent_task_windows;
#[cfg(test)]
mod tests;

use rusqlite::{Connection, Row, params};

#[cfg(test)]
use super::super::WriteContext;
use super::super::{BoardError, invalid, require_plan, row_number, sql_error, sql_number};
use super::collection_reads::{count, sequence_window, validate_limit};
use crate::board::board_actor::BoardRecipient;
#[cfg(test)]
use crate::board::board_domain::board_collections::ClaimCursor;
use crate::board::board_domain::board_collections::{TaskCeiling, TaskPage};
#[cfg(test)]
use crate::board::board_ids::{EntryId, RepoKey};
use crate::board::board_ids::{EventSeq, PlanId, TaskId};
use crate::board::board_protocol::{
    BoardOp, BoardReply, BoardResult, DoneTasksPage, ReadScope, TaskCursor, TaskOrder, TaskRecord,
};
use crate::board::board_vocabulary::{PlanTitle, TaskColumn};
pub(in crate::board::local_board) use claim_windows::{claim_window, claims_page};
pub(in crate::board::local_board) use overview_task_windows::overview_task_window;
use recent_task_windows::recent_task_window;

const NESTED_PAGE_LIMIT: usize = 200;
pub(in crate::board::local_board) const TASK_SELECT: &str = concat!(
    "SELECT t.ordinal,t.title,t.column_name,t.assignee,t.section,t.seq,completion.created_at,t.plan_id ",
    "FROM tasks t LEFT JOIN events completion ON completion.seq=t.seq AND t.column_name='done'"
);

pub(in crate::board::local_board) fn task_row(
    row: &Row<'_>,
    plan: PlanId,
) -> Result<TaskRecord, BoardError> {
    let assignee: Option<String> = row.get(3).map_err(sql_error)?;
    Ok(TaskRecord {
        id: TaskId::new(plan, row_number(row, 0).map_err(sql_error)?).map_err(BoardError::from)?,
        title: PlanTitle::new(row.get::<_, String>(1).map_err(sql_error)?)
            .map_err(BoardError::from)?,
        column: row
            .get::<_, String>(2)
            .map_err(sql_error)?
            .parse()
            .map_err(BoardError::from)?,
        assignee: assignee
            .as_deref()
            .map(BoardRecipient::parse)
            .transpose()
            .map_err(BoardError::from)?,
        section: row.get(4).map_err(sql_error)?,
        seq: EventSeq::new(row_number(row, 5).map_err(sql_error)?),
        done_at: row.get(6).map_err(sql_error)?,
    })
}

#[derive(Clone, Copy, Default)]
pub(in crate::board::local_board) struct TaskSelection {
    pub column: Option<TaskColumn>,
    pub order: TaskOrder,
    pub before: Option<TaskCursor>,
}

pub(in crate::board::local_board) fn tasks_page(
    conn: &Connection,
    plan: PlanId,
    after: Option<TaskId>,
    ceiling: Option<TaskCeiling>,
    through: Option<EventSeq>,
    limit: usize,
    selection: TaskSelection,
) -> Result<BoardReply, BoardError> {
    BoardOp::Tasks {
        plan,
        column: selection.column,
        order: selection.order,
        before: selection.before,
        after,
        ceiling,
        through,
        limit,
    }
    .validate()
    .map_err(BoardError::from)?;
    let page = if selection.order == TaskOrder::RecentFirst {
        require_plan(conn, plan)?;
        let (_, through) = sequence_window(conn, None, through)?;
        let recent = recent_task_window(
            conn,
            &ReadScope::All,
            Some(plan),
            selection.column,
            selection.before,
            limit,
        )?;
        TaskPage {
            plan,
            column: selection.column,
            order: selection.order,
            tasks: recent.tasks,
            before: selection.before,
            next_before: recent.next_before,
            omitted: recent.omitted,
            after: None,
            next_after: None,
            ceiling: task_ceiling(conn, plan)?,
            through,
        }
    } else {
        task_window(conn, plan, after, ceiling, through, limit, selection.column)?
    };
    Ok(BoardReply::new("local", BoardResult::Tasks(page)))
}

pub(in crate::board::local_board) fn done_tasks_page(
    conn: &Connection,
    scope: &ReadScope,
    before: Option<TaskCursor>,
    limit: usize,
    server_now: i64,
) -> Result<BoardReply, BoardError> {
    let recent = recent_task_window(conn, scope, None, Some(TaskColumn::Done), before, limit)?;
    Ok(BoardReply::new(
        "local",
        BoardResult::DoneTasks(DoneTasksPage {
            scope: scope.clone(),
            tasks: recent.tasks,
            before,
            next_before: recent.next_before,
            omitted: recent.omitted,
            server_now,
        }),
    ))
}

fn task_ceiling(conn: &Connection, plan: PlanId) -> Result<TaskCeiling, BoardError> {
    let ordinal = conn
        .query_row(
            "SELECT coalesce(max(ordinal),0) FROM tasks WHERE plan_id=?1",
            [sql_number(plan.get())],
            |row| row_number(row, 0),
        )
        .map_err(sql_error)?;
    Ok(TaskCeiling { plan, ordinal })
}

pub(in crate::board::local_board) fn task_window(
    conn: &Connection,
    plan: PlanId,
    after: Option<TaskId>,
    ceiling: Option<TaskCeiling>,
    through: Option<EventSeq>,
    limit: usize,
    column: Option<TaskColumn>,
) -> Result<TaskPage, BoardError> {
    validate_limit(limit, NESTED_PAGE_LIMIT)?;
    if let Some(after) = after {
        after.validate().map_err(BoardError::from)?;
        if after.plan != plan {
            return Err(invalid(
                "invalid_reference",
                "task cursor belongs to a different plan",
            ));
        }
    }
    if let Some(ceiling) = ceiling {
        ceiling.validate().map_err(BoardError::from)?;
        if ceiling.plan != plan {
            return Err(invalid(
                "invalid_reference",
                "task ceiling belongs to a different plan",
            ));
        }
    }
    require_plan(conn, plan)?;
    let ceiling = match ceiling {
        Some(ceiling) => ceiling,
        None => task_ceiling(conn, plan)?,
    };
    let (_, through) = sequence_window(conn, None, through)?;
    let parameters = params![
        sql_number(plan.get()),
        sql_number(after.map_or(0, |task| task.ordinal)),
        sql_number(ceiling.ordinal),
        column.map(|column| column.to_string()),
    ];
    let total = count(
        conn,
        "SELECT count(*) FROM tasks WHERE plan_id=?1 AND ordinal>?2 AND ordinal<=?3 AND (?4 IS NULL OR column_name=?4)",
        parameters,
    )?;
    let mut statement = conn
        .prepare(&format!(
            "{TASK_SELECT} WHERE t.plan_id=?1 AND t.ordinal>?2 AND t.ordinal<=?3 \
             AND (?4 IS NULL OR t.column_name=?4) ORDER BY t.ordinal LIMIT ?5"
        ))
        .map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            sql_number(plan.get()),
            sql_number(after.map_or(0, |task| task.ordinal)),
            sql_number(ceiling.ordinal),
            column.map(|column| column.to_string()),
            sql_number((limit + 1) as u64)
        ])
        .map_err(sql_error)?;
    let mut tasks = Vec::with_capacity((limit + 1).min(total));
    while let Some(row) = rows.next().map_err(sql_error)? {
        tasks.push(task_row(row, plan)?);
    }
    let next_after = if tasks.len() > limit {
        tasks.truncate(limit);
        tasks.last().map(|task| task.id)
    } else {
        None
    };
    Ok(TaskPage {
        plan,
        column,
        order: TaskOrder::Ordinal,
        omitted: total.saturating_sub(tasks.len()),
        tasks,
        after,
        before: None,
        ceiling,
        through,
        next_after,
        next_before: None,
    })
}
