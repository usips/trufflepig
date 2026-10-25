//! Worker-binding checks for task card writes.

use super::{BoardError, sql_error, sql_number};
use crate::board::board_ids::TaskId;
use rusqlite::{Transaction, params};

pub(in crate::board::local_board) fn is_bound_worker(
    tx: &Transaction<'_>,
    task: TaskId,
    actor_id: i64,
) -> Result<bool, BoardError> {
    tx.query_row(
        concat!(
            "SELECT EXISTS(SELECT 1 FROM claim_workers w JOIN claims c ON c.id=w.claim_id ",
            "WHERE w.plan_id=?1 AND w.task_ordinal=?2 AND w.actor_id=?3 ",
            "AND w.ended_at IS NULL AND c.ended_at IS NULL)"
        ),
        params![
            sql_number(task.plan.get()),
            sql_number(task.ordinal),
            actor_id
        ],
        |row| row.get(0),
    )
    .map_err(sql_error)
}
