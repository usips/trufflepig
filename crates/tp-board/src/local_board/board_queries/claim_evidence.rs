//! Bounded claim evidence shared by task and collection reads.
use super::{progress_reads, worker_reads};
use crate::board::board_ids::{EntryId, EventSeq, TaskId};
use crate::board::board_vocabulary::EntryText;
use crate::board::local_board::{
    BoardError, WriteContext, actor_from_row, delegated_actor_from_row, row_number, sql_error,
    sql_number,
};
use crate::board::{board_actor::claim_vendor, board_protocol::*};
use rusqlite::{Connection, OptionalExtension, params};

pub(in crate::board::local_board) fn question_summary(
    conn: &Connection,
    task: TaskId,
    through: EventSeq,
) -> Result<(usize, Option<EntryId>, Option<i64>), BoardError> {
    let predicate = concat!(
        "e.kind='question' AND e.seq<=?2 AND (e.to_whom=?1 OR EXISTS(",
        "SELECT 1 FROM entry_refs r WHERE r.entry_id=e.id AND r.target=?1)) ",
        "AND NOT EXISTS(SELECT 1 FROM entries answer JOIN entry_refs r ON r.entry_id=answer.id ",
        "WHERE answer.kind='answer' AND answer.seq<=?2 AND r.target='E'||e.id) ",
        "AND NOT EXISTS(SELECT 1 FROM entries replacement WHERE replacement.supersedes=e.id AND replacement.seq<=?2)"
    );
    let count: i64 = conn
        .query_row(
            &format!("SELECT count(*) FROM entries e WHERE {predicate}"),
            params![task.to_string(), sql_number(through.get())],
            |r| r.get(0),
        )
        .map_err(sql_error)?;
    let oldest: Option<(i64, i64)> = conn.query_row(&format!(
        "SELECT e.id,e.created_at FROM entries e WHERE {predicate} ORDER BY e.created_at,e.id LIMIT 1"),
        params![task.to_string(), sql_number(through.get())], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional().map_err(sql_error)?;
    let entry = oldest
        .map(|(id, _)| EntryId::new(id as u64).map_err(BoardError::from))
        .transpose()?;
    Ok((count.max(0) as usize, entry, oldest.map(|(_, at)| at)))
}

pub(in crate::board::local_board) fn hydrate_claim(
    conn: &Connection,
    ctx: &WriteContext,
    claim: &mut ClaimRecord,
    through: EventSeq,
) -> Result<(), BoardError> {
    (claim.workers, claim.workers_omitted) = worker_reads::workers_for_claim(
        conn,
        claim.task,
        claim.entry,
        ctx.now,
        ctx.claim_ttl_secs,
    )?;
    claim.progress = progress_reads::progress_for_claim(conn, claim.entry, None)?;
    (
        claim.open_questions,
        claim.oldest_open_question,
        claim.oldest_open_question_at,
    ) = question_summary(conn, claim.task, through)?;
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM commit_tasks WHERE plan_id=?1 AND task_ordinal=?2 AND claim_entry=?3",
        params![sql_number(claim.task.plan.get()), sql_number(claim.task.ordinal), sql_number(claim.entry.get())],
        |r| r.get(0)).map_err(sql_error)?;
    claim.commit_count = count.max(0) as usize;
    let latest: Option<(String,i64)> = conn.query_row(concat!(
        "SELECT c.oid,c.committed_at FROM commit_tasks t JOIN commits c ON c.repo_key=t.repo_key AND c.oid=t.oid ",
        "WHERE t.plan_id=?1 AND t.task_ordinal=?2 AND t.claim_entry=?3 ORDER BY c.committed_at DESC,c.oid LIMIT 1"),
        params![sql_number(claim.task.plan.get()), sql_number(claim.task.ordinal), sql_number(claim.entry.get())],
        |r| Ok((r.get(0)?,r.get(1)?))).optional().map_err(sql_error)?;
    claim.latest_commit = latest
        .as_ref()
        .map(|(oid, _)| tp_core::identity::GitOid::parse(oid).map_err(BoardError::from))
        .transpose()?;
    claim.latest_commit_at = latest.map(|(_, at)| at);
    Ok(())
}

pub(in crate::board::local_board) fn claims_for_task(
    conn: &Connection,
    ctx: &WriteContext,
    task: TaskId,
    through: EventSeq,
    limit: usize,
) -> Result<(Vec<ClaimRecord>, usize), BoardError> {
    let count: i64 = conn
        .query_row(
            concat!(
                "SELECT count(*) FROM claims c JOIN entries e ON e.id=c.entry_id ",
                "WHERE c.plan_id=?1 AND c.task_ordinal=?2 AND e.seq<=?3"
            ),
            params![
                sql_number(task.plan.get()),
                sql_number(task.ordinal),
                sql_number(through.get())
            ],
            |r| r.get(0),
        )
        .map_err(sql_error)?;
    let mut stmt = conn.prepare(concat!(
        "SELECT a.user,a.host,a.harness,a.session,c.entry_id,c.scope,c.claimed_at,c.last_active,c.ended_at,c.end_reason,",
        "e.model,e.effort,d.user,d.host,d.harness,d.session FROM claims c JOIN actors a ON a.id=c.actor_id ",
        "JOIN entries e ON e.id=c.entry_id LEFT JOIN actors d ON d.id=c.delegated_by ",
        "WHERE c.plan_id=?1 AND c.task_ordinal=?2 AND e.seq<=?3 ORDER BY c.id DESC LIMIT ?4"))
        .map_err(sql_error)?;
    let mut rows = stmt
        .query(params![
            sql_number(task.plan.get()),
            sql_number(task.ordinal),
            sql_number(through.get()),
            limit.min(21) as i64
        ])
        .map_err(sql_error)?;
    let mut claims = Vec::with_capacity(limit.min(21).min(count.max(0) as usize));
    while let Some(row) = rows.next().map_err(sql_error)? {
        let actor = actor_from_row(row, 0).map_err(sql_error)?;
        let model: Option<String> = row.get(10).map_err(sql_error)?;
        let ended_at: Option<i64> = row.get(8).map_err(sql_error)?;
        let reason: Option<String> = row.get(9).map_err(sql_error)?;
        let last_active: i64 = row.get(7).map_err(sql_error)?;
        claims.push(ClaimRecord {
            task,
            vendor: claim_vendor(&actor.harness, model.as_deref()),
            actor,
            entry: EntryId::new(row_number(row, 4).map_err(sql_error)?)
                .map_err(BoardError::from)?,
            scope: EntryText::new(row.get::<_, String>(5).map_err(sql_error)?)
                .map_err(BoardError::from)?,
            claimed_at: row.get(6).map_err(sql_error)?,
            last_active,
            ended_at,
            end_reason: reason
                .as_deref()
                .map(ClaimEndReason::parse)
                .transpose()
                .map_err(BoardError::from)?,
            stale: ended_at.is_none()
                && last_active < ctx.now.saturating_sub(ctx.claim_ttl_secs.max(0)),
            model,
            effort: row.get(11).map_err(sql_error)?,
            delegated_by: delegated_actor_from_row(row, 12).map_err(sql_error)?,
            workers: Vec::new(),
            workers_omitted: 0,
            progress: None,
            open_questions: 0,
            oldest_open_question: None,
            oldest_open_question_at: None,
            commit_count: 0,
            latest_commit: None,
            latest_commit_at: None,
        });
    }
    let omitted = (count.max(0) as usize).saturating_sub(claims.len());
    Ok((claims, omitted))
}
