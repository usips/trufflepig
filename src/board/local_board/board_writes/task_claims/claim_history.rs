//! Stored task cards and immutable lease history windows.

use super::*;

pub(in crate::board::local_board) fn read_tasks(
    conn: &Connection,
    plan: PlanId,
) -> Result<Vec<TaskRecord>, BoardError> {
    let mut statement = conn
        .prepare("SELECT ordinal,title,column_name,assignee,section,seq FROM tasks WHERE plan_id=?1 ORDER BY ordinal")
        .map_err(sql_error)?;
    let mut rows = statement
        .query([sql_number(plan.get())])
        .map_err(sql_error)?;
    let mut tasks = Vec::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        let title: String = row.get(1).map_err(sql_error)?;
        let column: String = row.get(2).map_err(sql_error)?;
        let assignee: Option<String> = row.get(3).map_err(sql_error)?;
        tasks.push(TaskRecord {
            id: TaskId::new(plan, row_number(row, 0).map_err(sql_error)?)
                .map_err(BoardError::from)?,
            title: PlanTitle::new(title).map_err(BoardError::from)?,
            column: column.parse().map_err(BoardError::from)?,
            assignee: assignee
                .as_deref()
                .map(BoardRecipient::parse)
                .transpose()
                .map_err(BoardError::from)?,
            section: row.get(4).map_err(sql_error)?,
            seq: EventSeq::new(row_number(row, 5).map_err(sql_error)?),
        });
    }
    Ok(tasks)
}

const CLAIM_SELECT: &str = concat!(
    "SELECT c.actor_id,c.task_ordinal,a.user,a.host,a.harness,a.session,c.entry_id,c.scope,",
    "c.claimed_at,c.last_active,c.ended_at,c.end_reason,e.model,e.effort,",
    "d.user,d.host,d.harness,d.session FROM claims c ",
    "JOIN actors a ON a.id=c.actor_id JOIN entries e ON e.id=c.entry_id ",
    "LEFT JOIN actors d ON d.id=c.delegated_by"
);

pub(in crate::board::local_board) fn active_claim(
    conn: &Connection,
    task: TaskId,
    now: i64,
    ttl: i64,
) -> Result<Option<StoredClaim>, BoardError> {
    let sql =
        format!("{CLAIM_SELECT} WHERE c.plan_id=?1 AND c.task_ordinal=?2 AND c.ended_at IS NULL");
    let mut statement = conn.prepare(&sql).map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            sql_number(task.plan.get()),
            sql_number(task.ordinal)
        ])
        .map_err(sql_error)?;
    rows.next()
        .map_err(sql_error)?
        .map(|row| claim_from_row(row, task.plan, now, ttl))
        .transpose()
}

pub(in crate::board::local_board) fn read_claims_window(
    conn: &Connection,
    plan: PlanId,
    start: i64,
    end: i64,
    now: i64,
    ttl: i64,
) -> Result<Vec<ClaimRecord>, BoardError> {
    let sql = format!(
        concat!(
            "{CLAIM_SELECT} WHERE c.plan_id=?1 AND c.claimed_at<=?3 ",
            "AND (c.ended_at IS NULL OR c.ended_at>=?2) ORDER BY c.claimed_at,c.id"
        ),
        CLAIM_SELECT = CLAIM_SELECT
    );
    let mut statement = conn.prepare(&sql).map_err(sql_error)?;
    let mut rows = statement
        .query(params![sql_number(plan.get()), start, end])
        .map_err(sql_error)?;
    let mut claims = Vec::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        claims.push(claim_from_row(row, plan, now, ttl)?.record);
    }
    Ok(claims)
}

fn claim_from_row(
    row: &Row<'_>,
    plan: PlanId,
    now: i64,
    ttl: i64,
) -> Result<StoredClaim, BoardError> {
    let scope: String = row.get(7).map_err(sql_error)?;
    let last_active: i64 = row.get(9).map_err(sql_error)?;
    let ended_at: Option<i64> = row.get(10).map_err(sql_error)?;
    let reason: Option<String> = row.get(11).map_err(sql_error)?;
    let end_reason = reason
        .as_deref()
        .map(ClaimEndReason::parse)
        .transpose()
        .map_err(|error| invalid("board_unavailable", error.to_string()))?;
    Ok(StoredClaim {
        actor_id: row.get(0).map_err(sql_error)?,
        record: ClaimRecord {
            task: TaskId::new(plan, row_number(row, 1).map_err(sql_error)?)
                .map_err(BoardError::from)?,
            actor: actor_from_row(row, 2).map_err(sql_error)?,
            entry: EntryId::new(row_number(row, 6).map_err(sql_error)?)
                .map_err(BoardError::from)?,
            scope: EntryText::new(scope).map_err(BoardError::from)?,
            claimed_at: row.get(8).map_err(sql_error)?,
            last_active,
            ended_at,
            end_reason,
            stale: ended_at.is_none() && last_active < now.saturating_sub(ttl.max(0)),
            model: row.get(12).map_err(sql_error)?,
            effort: row.get(13).map_err(sql_error)?,
            delegated_by: delegated_actor_from_row(row, 14).map_err(sql_error)?,
        },
    })
}
