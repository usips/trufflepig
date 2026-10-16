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
    let task_record = task_row_result
        .transpose()?
        .ok_or_else(|| invalid("invalid_reference", format!("unknown task {task}")))?;

    let (mut claims, mut claims_omitted) =
        claim_evidence::claims_for_task(conn, ctx, task, through, TASK_CLAIM_LIMIT)?;
    for claim in &mut claims {
        claim_evidence::hydrate_claim(conn, ctx, claim, through)?;
    }
    let current_claim_index = claims.iter().position(|claim| claim.ended_at.is_none());
    let current_claim = current_claim_index.map(|index| claims.remove(index));
    // Claim history is newest first and excludes the separately exposed active claim.
    let history_limit = TASK_CLAIM_LIMIT.saturating_sub(1);
    if current_claim.is_none() {
        claims_omitted = claims_omitted.saturating_add(claims.len().saturating_sub(history_limit));
    }
    claims.truncate(history_limit);

    let (open_questions, oldest_open_question, oldest_open_question_at) =
        claim_evidence::question_summary(conn, task, through)?;

    let target = task.to_string();
    let task_entry_predicate = concat!(
        "EXISTS(SELECT 1 FROM entry_refs direct_ref WHERE direct_ref.entry_id=e.id AND direct_ref.target=?1) ",
        "OR EXISTS(SELECT 1 FROM entry_refs reply_ref ",
        "JOIN entries parent ON reply_ref.target='E'||parent.id ",
        "JOIN entry_refs parent_ref ON parent_ref.entry_id=parent.id AND parent_ref.target=?1 ",
        "WHERE reply_ref.entry_id=e.id)"
    );
    let entries_total: i64 = conn
        .query_row(
            &format!("SELECT count(*) FROM entries e WHERE e.seq<=?2 AND ({task_entry_predicate})"),
            params![target, sql_number(through.get())],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let entry_records = entries(
        conn,
        &format!(
            "SELECT e.id FROM entries e WHERE e.seq<=?2 AND ({task_entry_predicate}) \
             ORDER BY e.seq DESC,e.id DESC LIMIT ?3"
        ),
        params![target, sql_number(through.get()), TASK_ENTRY_LIMIT as i64],
    )?;
    let entries_omitted = usize::try_from(entries_total)
        .map_err(|_| {
            invalid(
                "board_unavailable",
                "task entry count exceeds platform range",
            )
        })?
        .saturating_sub(entry_records.len());

    let (commits, total_commits, latest_commit_at) =
        board_commits::linked_commits_for_task(conn, task, TASK_COMMIT_LIMIT)?;
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
