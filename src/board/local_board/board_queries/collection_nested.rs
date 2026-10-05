//! Task ordinal ceilings and claim source sequences bound current-state collection pages.

mod claim_windows;
#[cfg(test)]
mod tests;

use rusqlite::{Connection, params};

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
use crate::board::board_protocol::{BoardReply, BoardResult, TaskRecord};
use crate::board::board_vocabulary::PlanTitle;
pub(in crate::board::local_board) use claim_windows::{claim_window, claims_page};

const NESTED_PAGE_LIMIT: usize = 200;

pub(in crate::board::local_board) fn tasks_page(
    conn: &Connection,
    plan: PlanId,
    after: Option<TaskId>,
    ceiling: Option<TaskCeiling>,
    through: Option<EventSeq>,
    limit: usize,
) -> Result<BoardReply, BoardError> {
    if ceiling.is_none() && (after.is_some() || through.is_some()) {
        return Err(invalid(
            "invalid_options",
            "task continuation requires its captured ceiling",
        ));
    }
    Ok(BoardReply::new(
        "local",
        BoardResult::Tasks(task_window(conn, plan, after, ceiling, through, limit)?),
    ))
}

pub(in crate::board::local_board) fn task_window(
    conn: &Connection,
    plan: PlanId,
    after: Option<TaskId>,
    ceiling: Option<TaskCeiling>,
    through: Option<EventSeq>,
    limit: usize,
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
        None => {
            let ordinal = conn
                .query_row(
                    "SELECT coalesce(max(ordinal),0) FROM tasks WHERE plan_id=?1",
                    [sql_number(plan.get())],
                    |row| row_number(row, 0),
                )
                .map_err(sql_error)?;
            TaskCeiling { plan, ordinal }
        }
    };
    let (_, through) = sequence_window(conn, None, through)?;
    let parameters = params![
        sql_number(plan.get()),
        sql_number(after.map_or(0, |task| task.ordinal)),
        sql_number(ceiling.ordinal)
    ];
    let total = count(
        conn,
        "SELECT count(*) FROM tasks WHERE plan_id=?1 AND ordinal>?2 AND ordinal<=?3",
        parameters,
    )?;
    let mut statement = conn
        .prepare(concat!(
            "SELECT ordinal,title,column_name,assignee,section,seq FROM tasks WHERE plan_id=?1 ",
            "AND ordinal>?2 AND ordinal<=?3 ORDER BY ordinal LIMIT ?4"
        ))
        .map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            sql_number(plan.get()),
            sql_number(after.map_or(0, |task| task.ordinal)),
            sql_number(ceiling.ordinal),
            sql_number((limit + 1) as u64)
        ])
        .map_err(sql_error)?;
    let mut tasks = Vec::with_capacity((limit + 1).min(total));
    while let Some(row) = rows.next().map_err(sql_error)? {
        let assignee: Option<String> = row.get(3).map_err(sql_error)?;
        tasks.push(TaskRecord {
            id: TaskId::new(plan, row_number(row, 0).map_err(sql_error)?)
                .map_err(BoardError::from)?,
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
        });
    }
    let next_after = if tasks.len() > limit {
        tasks.truncate(limit);
        tasks.last().map(|task| task.id)
    } else {
        None
    };
    Ok(TaskPage {
        plan,
        omitted: total.saturating_sub(tasks.len()),
        tasks,
        after,
        ceiling,
        through,
        next_after,
    })
}
