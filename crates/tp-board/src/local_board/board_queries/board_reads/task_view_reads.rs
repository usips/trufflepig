//! Exact-task detail reads with bounded claim, entry, and commit evidence.

use super::*;
use crate::board::board_ids::TaskId;
use crate::board::board_protocol::TaskView;
use crate::board::local_board::board_queries::collection_nested::{TASK_SELECT, task_row};
use crate::board::local_board::board_queries::{board_commits, claim_evidence};
use rusqlite::OptionalExtension;

const TASK_ENTRY_LIMIT: usize = 50;
const TASK_CLAIM_LIMIT: usize = 21;
const TASK_COMMIT_LIMIT: usize = 50;

pub(in crate::board::local_board) fn task_view(
    conn: &Connection,
    ctx: &WriteContext,
    task: TaskId,
) -> Result<TaskView, BoardError> {
    let through = crate::board::local_board::max_seq(conn)?;
    let task_row_result = conn
        .query_row(
            &format!("{TASK_SELECT} WHERE t.plan_id=?1 AND t.ordinal=?2 AND t.seq<=?3"),
            params![
                sql_number(task.plan.get()),
                sql_number(task.ordinal),
                sql_number(through.get()),
            ],
            |row| Ok(task_row(row, task.plan)),
        )
        .optional()
        .map_err(sql_error)?;
    let task_record = task_row_result.transpose()?.ok_or_else(|| {
        invalid(
            BoardErrorCode::InvalidReference,
            format!("unknown task {task}"),
        )
    })?;

    let mut lineage = task_lineage::task_ids(conn, task)?;
    let canonical = task_lineage::canonical_task(conn, task)?;
    if let Some(position) = lineage.iter().position(|item| *item == canonical) {
        lineage.swap_remove(position);
        lineage.push(canonical);
    }
    let mut claims = Vec::with_capacity(lineage.len().min(TASK_CLAIM_LIMIT));
    let mut total_claims = 0usize;
    for lineage_task in &lineage {
        let (mut task_claims, omitted) =
            claim_evidence::claims_for_task(conn, ctx, *lineage_task, through, TASK_CLAIM_LIMIT)?;
        total_claims = total_claims.saturating_add(task_claims.len().saturating_add(omitted));
        claims.append(&mut task_claims);
    }
    claims.sort_by(|left, right| {
        right
            .claimed_at
            .cmp(&left.claimed_at)
            .then_with(|| right.entry.get().cmp(&left.entry.get()))
    });
    claims.truncate(TASK_CLAIM_LIMIT);
    for claim in &mut claims {
        claim_evidence::hydrate_claim(conn, ctx, claim, through)?;
    }
    let current_claim_index = claims.iter().position(|claim| claim.ended_at.is_none());
    let current_claim = current_claim_index.map(|index| claims.remove(index));
    // Claim history is newest first and excludes the separately exposed active claim.
    let history_limit = TASK_CLAIM_LIMIT.saturating_sub(1);
    claims.truncate(history_limit);
    let retained_claims = claims.len() + usize::from(current_claim.is_some());
    let claims_omitted = total_claims.saturating_sub(retained_claims);

    let (open_questions, oldest_open_question, oldest_open_question_at) =
        task_lineage::open_question_summary(conn, task, through)?;
    let task_entry_predicate = task_lineage::entry_predicate("e.id");
    let task_lineage_cte = task_lineage::TASK_LINEAGE_CTE;
    let entries_total: i64 = conn
        .query_row(
            &format!(
                "{task_lineage_cte} SELECT count(*) FROM entries e WHERE e.seq<=?3 AND ({task_entry_predicate})"
            ),
            params![
                sql_number(task.plan.get()),
                sql_number(task.ordinal),
                sql_number(through.get())
            ],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let entry_records = entries(
        conn,
        &format!(
            "{task_lineage_cte} SELECT e.id FROM entries e WHERE e.seq<=?3 AND ({task_entry_predicate}) \
             ORDER BY e.seq DESC,e.id DESC LIMIT ?4"
        ),
        params![
            sql_number(task.plan.get()),
            sql_number(task.ordinal),
            sql_number(through.get()),
            TASK_ENTRY_LIMIT as i64
        ],
    )?;
    let entries_omitted = usize::try_from(entries_total)
        .map_err(|_| {
            invalid(
                BoardErrorCode::BoardUnavailable,
                "task entry count exceeds platform range",
            )
        })?
        .saturating_sub(entry_records.len());

    let mut commits = Vec::with_capacity(lineage.len().min(TASK_COMMIT_LIMIT));
    let mut total_commits = 0usize;
    let mut latest_commit_at = None;
    for lineage_task in &lineage {
        let (mut task_commits, task_total, task_latest) =
            board_commits::linked_commits_for_task(conn, *lineage_task, TASK_COMMIT_LIMIT)?;
        total_commits = total_commits.saturating_add(task_total);
        latest_commit_at = latest_commit_at.max(task_latest);
        commits.append(&mut task_commits);
    }
    commits.sort_by(|left, right| right.committed_at.cmp(&left.committed_at));
    commits.truncate(TASK_COMMIT_LIMIT);
    let commits_omitted = total_commits.saturating_sub(commits.len());

    Ok(TaskView {
        task: task_record,
        current_claim,
        claim_history: claims,
        entries: entry_records,
        commits,
        open_questions,
        oldest_open_question,
        oldest_open_question_at,
        latest_commit_at,
        through,
        server_now: ctx.now,
        claim_ttl_secs: ctx.claim_ttl_secs.max(0) as u64,
        claims_omitted,
        entries_omitted,
        commits_omitted,
    })
}
