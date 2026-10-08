//! Current-state task pages continue by completion sequence and numeric task identity.
use super::{NESTED_PAGE_LIMIT, TASK_SELECT, task_row};
use crate::board::board_ids::PlanId;
use crate::board::board_protocol::{BoardError, ReadScope, TaskCursor, TaskRecord};
use crate::board::board_vocabulary::TaskColumn;
use crate::board::local_board::board_queries::collection_reads::{count, validate_limit};
use crate::board::local_board::board_queries::read_scope_sql::ScopeSql;
use crate::board::local_board::{row_number, sql_error, sqlite_id};
use rusqlite::{Connection, params_from_iter, types::Value};

pub(super) struct RecentTaskWindow {
    pub tasks: Vec<TaskRecord>,
    pub next_before: Option<TaskCursor>,
    pub omitted: usize,
}

pub(super) fn recent_task_window(
    conn: &Connection,
    scope: &ReadScope,
    plan: Option<PlanId>,
    column: Option<TaskColumn>,
    before: Option<TaskCursor>,
    limit: usize,
) -> Result<RecentTaskWindow, BoardError> {
    validate_limit(limit, NESTED_PAGE_LIMIT)?;
    if let Some(before) = before {
        before.validate().map_err(BoardError::from)?;
    }
    let scope = ScopeSql::new(scope)?;
    let predicate = ScopeSql::plan_predicate("t.plan_id", 1, 2);
    let filter = format!(
        "{predicate} AND (?3 IS NULL OR t.plan_id=?3) \
         AND (?4 IS NULL OR t.column_name=?4) \
         AND (?5 IS NULL OR (t.seq,t.plan_id,t.ordinal)<(?5,?6,?7))"
    );
    let values = [
        Value::Integer(scope.kind),
        Value::Text(scope.keys_json),
        number(plan.map(PlanId::get))?,
        column.map_or(Value::Null, |column| Value::Text(column.to_string())),
        number(before.map(|cursor| cursor.seq.get()))?,
        number(before.map(|cursor| cursor.id.plan.get()))?,
        number(before.map(|cursor| cursor.id.ordinal))?,
        Value::Integer(sqlite_id((limit + 1) as u64)?),
    ];
    let total = count(
        conn,
        &format!("SELECT count(*) FROM tasks t WHERE {filter}"),
        params_from_iter(values[..7].iter()),
    )?;
    let mut statement = conn.prepare(&format!(
        "{TASK_SELECT} WHERE {filter} ORDER BY t.seq DESC,t.plan_id DESC,t.ordinal DESC LIMIT ?8"
    )).map_err(sql_error)?;
    let mut rows = statement
        .query(params_from_iter(values.iter()))
        .map_err(sql_error)?;
    let mut tasks = Vec::with_capacity((limit + 1).min(total));
    while let Some(row) = rows.next().map_err(sql_error)? {
        let plan = PlanId::new(row_number(row, 7).map_err(sql_error)?).map_err(BoardError::from)?;
        tasks.push(task_row(row, plan)?);
    }
    let next_before = if tasks.len() > limit {
        tasks.truncate(limit);
        tasks.last().map(|task| TaskCursor {
            seq: task.seq,
            id: task.id,
        })
    } else {
        None
    };
    Ok(RecentTaskWindow {
        omitted: total.saturating_sub(tasks.len()),
        tasks,
        next_before,
    })
}

fn number(value: Option<u64>) -> Result<Value, BoardError> {
    value.map_or(Ok(Value::Null), |value| {
        sqlite_id(value).map(Value::Integer)
    })
}
