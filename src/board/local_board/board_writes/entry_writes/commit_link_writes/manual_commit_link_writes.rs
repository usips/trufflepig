//! Manual commit links with linker attribution and durable replay receipts.

use super::super::commit_link_authority::{
    TaskLinkReceipt, plan_commit_entry, record_plan_link, require_link_authority, task_link_receipt,
};
use super::*;

/// Manual links name the linker and record `manual` provenance. Relinking the
/// same task replays the original receipt; linking another task of the plan
/// writes a new event without duplicating the plan link.
pub(in crate::board::local_board) fn link_commit(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    task: TaskId,
    commit: &LinkedCommit,
) -> Result<BoardReply, BoardError> {
    let plan = task.plan;
    require_link_authority(tx, &ctx.actor, plan)?;
    let task_exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE plan_id=?1 AND ordinal=?2)",
            params![sql_number(plan.get()), sql_number(task.ordinal)],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    if !task_exists {
        return Err(invalid("invalid_reference", format!("unknown task {task}")));
    }
    insert_commit_row(tx, commit)?;
    let existing_entry = plan_commit_entry(tx, commit, plan)?;
    if let Some(receipt) = task_link_receipt(tx, &commit.repo_key, commit.oid, task)? {
        let plan_entry = existing_entry.ok_or_else(|| {
            invalid(
                "invalid_state",
                format!("commit link for {task} has no plan-level entry"),
            )
        })?;
        let (seq, deduplicated) = match receipt {
            TaskLinkReceipt::Manual(seq) => (seq, true),
            TaskLinkReceipt::ManualWithoutEvent => {
                return Err(invalid(
                    "invalid_state",
                    format!("manual commit link for {task} has no stored event receipt"),
                ));
            }
            TaskLinkReceipt::Scan => (plan_entry.seq, true),
        };
        return Ok(BoardReply::new(
            "local",
            BoardResult::Change(BoardChange {
                entry: plan_entry.entry,
                seq,
                plan: Some(plan),
                revision: None,
                task: Some(task),
                deduplicated,
            }),
        ));
    }
    let plan_entry = match existing_entry {
        Some(entry) => entry,
        None => record_plan_link(tx, ctx, commit, plan)?,
    };
    tx.execute(
        "INSERT OR IGNORE INTO plan_repos(plan_id,repo_key) VALUES(?1,?2)",
        params![sql_number(plan.get()), commit.repo_key.as_str()],
    )
    .map_err(sql_error)?;
    let inserted = tx.execute(
        "INSERT OR IGNORE INTO commit_tasks(repo_key,oid,plan_id,task_ordinal,source) VALUES(?1,?2,?3,?4,'manual')",
        params![
            commit.repo_key.as_str(),
            commit.oid.as_str(),
            sql_number(plan.get()),
            sql_number(task.ordinal)
        ],
    ).map_err(sql_error)?;
    if inserted == 0 {
        return Err(invalid(
            "invalid_state",
            format!("commit link for {task} changed during its write"),
        ));
    }
    task_claims::refresh_commit_claims(
        tx,
        plan,
        task.ordinal,
        &commit.coauthors,
        commit.committed_at,
        ctx.now,
    )?;
    insert_event(
        tx,
        ctx,
        Some(plan),
        EntryKind::Commit,
        &plan_entry.entry.to_string(),
        None,
        &format!("linked {} to {task} by hand", commit.oid),
    )?;
    let linked = tx
        .execute(
            concat!(
                "UPDATE commit_tasks SET link_seq=?5 WHERE repo_key=?1 AND oid=?2 ",
                "AND plan_id=?3 AND task_ordinal=?4 AND source='manual' AND link_seq IS NULL"
            ),
            params![
                commit.repo_key.as_str(),
                commit.oid.as_str(),
                sql_number(plan.get()),
                sql_number(task.ordinal),
                sql_number(ctx.seq.get())
            ],
        )
        .map_err(sql_error)?;
    if linked != 1 {
        return Err(invalid(
            "invalid_state",
            format!("manual commit link for {task} could not store its event receipt"),
        ));
    }
    Ok(BoardReply::new(
        "local",
        BoardResult::Change(BoardChange {
            entry: plan_entry.entry,
            seq: ctx.seq,
            plan: Some(plan),
            revision: None,
            task: Some(task),
            deduplicated: false,
        }),
    ))
}
