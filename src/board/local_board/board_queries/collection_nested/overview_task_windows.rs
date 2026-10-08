//! Current active tasks by ordinal and recent completions by move sequence.
//! Done tasks never consume the active window's capacity.
use super::*;

const ACTIVE_TASKS: &str = "t.column_name IN ('todo','doing','review','blocked')";
const RECENT_DONE_LIMIT: usize = 5;

pub(in crate::board::local_board) struct OverviewTaskWindow {
    pub(in crate::board::local_board) tasks: Vec<TaskRecord>,
    pub(in crate::board::local_board) omitted: usize,
    pub(in crate::board::local_board) ceiling: TaskCeiling,
    pub(in crate::board::local_board) done_count: usize,
    pub(in crate::board::local_board) recent_done: Vec<TaskRecord>,
}

pub(in crate::board::local_board) fn overview_task_window(
    conn: &Connection,
    plan: PlanId,
    limit: usize,
) -> Result<OverviewTaskWindow, BoardError> {
    validate_limit(limit, NESTED_PAGE_LIMIT)?;
    require_plan(conn, plan)?;
    let ordinal = conn
        .query_row(
            "SELECT coalesce(max(ordinal),0) FROM tasks WHERE plan_id=?1",
            [sql_number(plan.get())],
            |row| row_number(row, 0),
        )
        .map_err(sql_error)?;
    let active_count = count(
        conn,
        &format!("SELECT count(*) FROM tasks t WHERE t.plan_id=?1 AND {ACTIVE_TASKS}"),
        [sql_number(plan.get())],
    )?;
    let done_count = count(
        conn,
        "SELECT count(*) FROM tasks WHERE plan_id=?1 AND column_name='done'",
        [sql_number(plan.get())],
    )?;
    let tasks = task_records_window(
        conn,
        plan,
        ACTIVE_TASKS,
        "t.ordinal",
        limit.min(active_count),
    )?;
    let recent_done = task_records_window(
        conn,
        plan,
        "t.column_name='done'",
        "t.seq DESC,t.ordinal DESC",
        RECENT_DONE_LIMIT.min(done_count),
    )?;
    Ok(OverviewTaskWindow {
        omitted: active_count.saturating_sub(tasks.len()),
        tasks,
        ceiling: TaskCeiling { plan, ordinal },
        done_count,
        recent_done,
    })
}

fn task_records_window(
    conn: &Connection,
    plan: PlanId,
    predicate: &str,
    order: &str,
    capacity: usize,
) -> Result<Vec<TaskRecord>, BoardError> {
    let mut statement = conn
        .prepare(&format!(
            "{TASK_SELECT} WHERE t.plan_id=?1 AND {predicate} ORDER BY {order} LIMIT ?2"
        ))
        .map_err(sql_error)?;
    let mut rows = statement
        .query(params![sql_number(plan.get()), sql_number(capacity as u64)])
        .map_err(sql_error)?;
    let mut tasks = Vec::with_capacity(capacity);
    while let Some(row) = rows.next().map_err(sql_error)? {
        tasks.push(task_row(row, plan)?);
    }
    Ok(tasks)
}
