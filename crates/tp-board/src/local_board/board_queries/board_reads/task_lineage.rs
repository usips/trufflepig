//! Immutable task succession lookups shared by task and reminder reads.

use super::*;
use crate::board_ids::{EventSeq, TaskId};
use rusqlite::OptionalExtension;

pub(super) const TASK_LINEAGE_CTE: &str = concat!(
    "WITH RECURSIVE task_lineage(plan_id,ordinal) AS (",
    "SELECT ?1,?2 ",
    "UNION ",
    "SELECT transfer.source_plan,transfer.source_ordinal ",
    "FROM task_transfers transfer JOIN task_lineage current ",
    "ON transfer.destination_plan=current.plan_id ",
    "AND transfer.destination_ordinal=current.ordinal ",
    "UNION ",
    "SELECT transfer.destination_plan,transfer.destination_ordinal ",
    "FROM task_transfers transfer JOIN task_lineage current ",
    "ON transfer.source_plan=current.plan_id ",
    "AND transfer.source_ordinal=current.ordinal) "
);

pub(super) fn entry_predicate(entry_id: &str) -> String {
    format!(
        concat!(
            "EXISTS(SELECT 1 FROM entry_refs direct_ref JOIN task_lineage current ",
            "ON direct_ref.target='P'||current.plan_id||'.'||current.ordinal ",
            "WHERE direct_ref.entry_id={entry_id}) OR ",
            "EXISTS(SELECT 1 FROM entries addressed JOIN task_lineage current ",
            "ON addressed.to_whom='P'||current.plan_id||'.'||current.ordinal ",
            "WHERE addressed.id={entry_id}) OR ",
            "EXISTS(SELECT 1 FROM entry_refs reply_ref JOIN entries parent ",
            "ON reply_ref.target='E'||parent.id JOIN entry_refs parent_ref ",
            "ON parent_ref.entry_id=parent.id JOIN task_lineage current ",
            "ON parent_ref.target='P'||current.plan_id||'.'||current.ordinal ",
            "WHERE reply_ref.entry_id={entry_id})"
        ),
        entry_id = entry_id
    )
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(in crate::local_board) struct TaskLineageLinks {
    pub(in crate::local_board) predecessor: Option<TaskId>,
    pub(in crate::local_board) successor: Option<TaskId>,
    pub(in crate::local_board) transfer_entry: Option<EntryId>,
}

pub(in crate::local_board) fn links(
    conn: &Connection,
    task: TaskId,
) -> Result<TaskLineageLinks, BoardError> {
    let row = conn
        .query_row(
            concat!(
                "SELECT incoming.source_plan,incoming.source_ordinal,incoming.entry_id,",
                "outgoing.destination_plan,outgoing.destination_ordinal ",
                "FROM (SELECT 1) seed ",
                "LEFT JOIN task_transfers incoming ON incoming.destination_plan=?1 ",
                "AND incoming.destination_ordinal=?2 ",
                "LEFT JOIN task_transfers outgoing ON outgoing.source_plan=?1 ",
                "AND outgoing.source_ordinal=?2"
            ),
            params![sql_number(task.plan.get()), sql_number(task.ordinal)],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                ))
            },
        )
        .map_err(sql_error)?;
    Ok(TaskLineageLinks {
        predecessor: task_id_from_pair(row.0, row.1)?,
        successor: task_id_from_pair(row.3, row.4)?,
        transfer_entry: row
            .2
            .map(sqlite_u64)
            .transpose()?
            .map(EntryId::new)
            .transpose()
            .map_err(BoardError::from)?,
    })
}

pub(in crate::local_board) fn task_ids(
    conn: &Connection,
    task: TaskId,
) -> Result<Vec<TaskId>, BoardError> {
    let count: i64 = conn
        .query_row(
            &format!("{TASK_LINEAGE_CTE} SELECT count(*) FROM task_lineage"),
            params![sql_number(task.plan.get()), sql_number(task.ordinal)],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let capacity = usize::try_from(count).map_err(|_| {
        invalid(
            BoardErrorCode::BoardUnavailable,
            "task lineage count exceeds platform range",
        )
    })?;
    let mut ids = Vec::with_capacity(capacity);
    let mut statement = conn
        .prepare(&format!(
            "{TASK_LINEAGE_CTE} SELECT plan_id,ordinal FROM task_lineage ORDER BY plan_id,ordinal"
        ))
        .map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            sql_number(task.plan.get()),
            sql_number(task.ordinal)
        ])
        .map_err(sql_error)?;
    while let Some(row) = rows.next().map_err(sql_error)? {
        ids.push(
            TaskId::new(
                PlanId::new(row_number(row, 0).map_err(sql_error)?).map_err(BoardError::from)?,
                row_number(row, 1).map_err(sql_error)?,
            )
            .map_err(BoardError::from)?,
        );
    }
    Ok(ids)
}

pub(in crate::local_board) fn canonical_task(
    conn: &Connection,
    task: TaskId,
) -> Result<TaskId, BoardError> {
    let terminal = conn
        .query_row(
            &format!(
                "{TASK_LINEAGE_CTE} SELECT current.plan_id,current.ordinal FROM task_lineage current \
                 WHERE NOT EXISTS(SELECT 1 FROM task_transfers transfer \
                   WHERE transfer.source_plan=current.plan_id AND transfer.source_ordinal=current.ordinal) \
                 ORDER BY current.plan_id,current.ordinal LIMIT 1"
            ),
            params![sql_number(task.plan.get()), sql_number(task.ordinal)],
            |row| Ok((row_number(row, 0)?, row_number(row, 1)?)),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| invalid(BoardErrorCode::BoardUnavailable, format!("task lineage for {task} has no terminal card")))?;
    TaskId::new(
        PlanId::new(terminal.0).map_err(BoardError::from)?,
        terminal.1,
    )
    .map_err(BoardError::from)
}

pub(in crate::local_board) fn has_live_claims_or_workers(
    conn: &Connection,
    task: TaskId,
) -> Result<bool, BoardError> {
    conn.query_row(
        concat!(
            "SELECT EXISTS(SELECT 1 FROM claims WHERE plan_id=?1 AND task_ordinal=?2 AND ended_at IS NULL) ",
            "OR EXISTS(SELECT 1 FROM claim_workers WHERE plan_id=?1 AND task_ordinal=?2 AND ended_at IS NULL)"
        ),
        params![sql_number(task.plan.get()), sql_number(task.ordinal)],
        |row| row.get(0),
    )
    .map_err(sql_error)
}

pub(in crate::local_board) fn plan_has_live_claims_or_workers(
    conn: &Connection,
    plan: PlanId,
) -> Result<bool, BoardError> {
    conn.query_row(
        concat!(
            "SELECT EXISTS(SELECT 1 FROM claims WHERE plan_id=?1 AND ended_at IS NULL) ",
            "OR EXISTS(SELECT 1 FROM claim_workers WHERE plan_id=?1 AND ended_at IS NULL)"
        ),
        [sql_number(plan.get())],
        |row| row.get(0),
    )
    .map_err(sql_error)
}

pub(super) fn open_question_summary(
    conn: &Connection,
    task: TaskId,
    through: EventSeq,
) -> Result<(usize, Option<EntryId>, Option<i64>), BoardError> {
    let predicate = concat!(
        "e.kind='question' AND e.seq<=?3 AND EXISTS(SELECT 1 FROM task_lineage current ",
        "WHERE e.to_whom='P'||current.plan_id||'.'||current.ordinal ",
        "OR EXISTS(SELECT 1 FROM entry_refs task_ref WHERE task_ref.entry_id=e.id ",
        "AND task_ref.target='P'||current.plan_id||'.'||current.ordinal)) ",
        "AND NOT EXISTS(SELECT 1 FROM entries answer JOIN entry_refs answer_ref ON answer_ref.entry_id=answer.id ",
        "WHERE answer.kind='answer' AND answer.seq<=?3 AND answer_ref.target='E'||e.id) ",
        "AND NOT EXISTS(SELECT 1 FROM entries replacement WHERE replacement.supersedes=e.id ",
        "AND replacement.seq<=?3)"
    );
    let count: i64 = conn
        .query_row(
            &format!("{TASK_LINEAGE_CTE} SELECT count(*) FROM entries e WHERE {predicate}"),
            params![
                sql_number(task.plan.get()),
                sql_number(task.ordinal),
                sql_number(through.get())
            ],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let oldest: Option<(i64, i64)> = conn
        .query_row(
            &format!(
                "{TASK_LINEAGE_CTE} SELECT e.id,e.created_at FROM entries e WHERE {predicate} \
                 ORDER BY e.created_at,e.id LIMIT 1"
            ),
            params![
                sql_number(task.plan.get()),
                sql_number(task.ordinal),
                sql_number(through.get())
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sql_error)?;
    let entry = oldest
        .map(|(id, _)| EntryId::new(sqlite_u64(id)?).map_err(BoardError::from))
        .transpose()?;
    Ok((count.max(0) as usize, entry, oldest.map(|(_, at)| at)))
}

pub(in crate::local_board) fn task_id_from_pair(
    plan: Option<i64>,
    ordinal: Option<i64>,
) -> Result<Option<TaskId>, BoardError> {
    match (plan, ordinal) {
        (None, None) => Ok(None),
        (Some(plan), Some(ordinal)) => Ok(Some(
            TaskId::new(
                PlanId::new(sqlite_u64(plan)?).map_err(BoardError::from)?,
                sqlite_u64(ordinal)?,
            )
            .map_err(BoardError::from)?,
        )),
        _ => Err(invalid(
            BoardErrorCode::BoardUnavailable,
            "stored task transfer has an incomplete task reference",
        )),
    }
}
