//! Authorized task link removal with append-only, durable unlink receipts.
use super::*;
use crate::identity::GitOid;
use commit_link_authority::{TaskLinkReceipt, require_link_authority, task_link_receipt};

pub(in crate::board::local_board) fn unlink_commit(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    oid: GitOid,
    task: TaskId,
) -> Result<BoardReply, BoardError> {
    require_link_authority(tx, &ctx.actor, task.plan)?;
    let Some(repo) = linked_repository(tx, oid, task)? else {
        return replay_unlink(tx, oid, task)?.ok_or_else(|| {
            invalid(
                "invalid_reference",
                format!("unknown commit link {oid} to {task}"),
            )
        });
    };
    let prior = task_link_receipt(tx, &repo, oid, task)?.ok_or_else(|| {
        invalid(
            "invalid_state",
            format!("commit link {oid} to {task} disappeared"),
        )
    })?;
    let (source, link_seq) = match prior {
        TaskLinkReceipt::Manual(seq) => ("manual", seq.to_string()),
        TaskLinkReceipt::ManualWithoutEvent => ("manual", "unknown".into()),
        TaskLinkReceipt::Scan => ("scan", "unknown".into()),
    };
    tx.execute(
        "DELETE FROM commit_tasks WHERE repo_key=?1 AND oid=?2 AND plan_id=?3 AND task_ordinal=?4",
        params![
            repo.as_str(),
            oid.as_str(),
            sql_number(task.plan.get()),
            sql_number(task.ordinal)
        ],
    )
    .map_err(sql_error)?;
    tx.execute(
        concat!(
            "DELETE FROM commit_plans WHERE repo_key=?1 AND oid=?2 AND plan_id=?3 ",
            "AND NOT EXISTS(SELECT 1 FROM commit_tasks WHERE repo_key=?1 AND oid=?2 AND plan_id=?3)"
        ),
        params![repo.as_str(), oid.as_str(), sql_number(task.plan.get())],
    )
    .map_err(sql_error)?;
    let body = format!("unlinked {oid} from {task} (source={source}; link_seq={link_seq})");
    let entry = insert_entry(
        tx,
        ctx,
        &EntryDraft {
            plan_id: Some(task.plan),
            kind: EntryKind::Unlinked,
            body: body.clone(),
            to_whom: None,
            supersedes: None,
            repo_key: Some(repo),
            state: None,
        },
    )?;
    for reference in [oid.to_string(), task.to_string()] {
        tx.execute(
            "INSERT OR IGNORE INTO entry_refs(entry_id,target) VALUES(?1,?2)",
            params![sql_number(entry.get()), reference],
        )
        .map_err(sql_error)?;
    }
    insert_event(
        tx,
        ctx,
        Some(task.plan),
        EntryKind::Unlinked,
        &entry.to_string(),
        None,
        &body,
    )?;
    Ok(ctx.change_reply(entry, Some(task.plan), None, Some(task)))
}

fn linked_repository(
    tx: &Transaction<'_>,
    oid: GitOid,
    task: TaskId,
) -> Result<Option<RepoKey>, BoardError> {
    let mut statement = tx
        .prepare(concat!(
            "SELECT repo_key FROM commit_tasks WHERE oid=?1 AND plan_id=?2 AND task_ordinal=?3 ",
            "ORDER BY repo_key LIMIT 2"
        ))
        .map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            oid.as_str(),
            sql_number(task.plan.get()),
            sql_number(task.ordinal)
        ])
        .map_err(sql_error)?;
    let Some(row) = rows.next().map_err(sql_error)? else {
        return Ok(None);
    };
    let repo: String = row.get(0).map_err(sql_error)?;
    if rows.next().map_err(sql_error)?.is_some() {
        return Err(ambiguous_link(oid, task));
    }
    RepoKey::parse(&repo).map(Some).map_err(BoardError::from)
}

/// A receipt must name a persisted commit in its repository and match both
/// protected unlink records; user-authored references cannot become receipts.
fn replay_unlink(
    tx: &Transaction<'_>,
    oid: GitOid,
    task: TaskId,
) -> Result<Option<BoardReply>, BoardError> {
    let mut statement = tx
        .prepare(concat!(
            "WITH unlink_receipts AS(SELECT e.repo_key,e.id,e.seq FROM entries e ",
            "JOIN entry_refs task ON task.entry_id=e.id AND task.target=?1 ",
            "JOIN entry_refs commit_ref ON commit_ref.entry_id=e.id AND commit_ref.target=?2 ",
            "JOIN commits c ON c.repo_key=e.repo_key AND c.oid=?2 ",
            "JOIN events v ON v.seq=e.seq AND v.kind='unlinked' AND v.plan_id=e.plan_id ",
            "AND v.subject='E'||e.id AND v.actor_id=e.actor_id ",
            "WHERE e.kind='unlinked' AND e.plan_id=?3) ",
            "SELECT r.id,r.seq FROM unlink_receipts r JOIN ",
            "(SELECT repo_key,MAX(seq) seq FROM unlink_receipts GROUP BY repo_key LIMIT 2) latest ",
            "ON latest.repo_key=r.repo_key AND latest.seq=r.seq"
        ))
        .map_err(sql_error)?;
    let mut rows = statement
        .query(params![
            task.to_string(),
            oid.as_str(),
            sql_number(task.plan.get())
        ])
        .map_err(sql_error)?;
    let Some(row) = rows.next().map_err(sql_error)? else {
        return Ok(None);
    };
    let entry: i64 = row.get(0).map_err(sql_error)?;
    let seq: i64 = row.get(1).map_err(sql_error)?;
    if rows.next().map_err(sql_error)?.is_some() {
        return Err(ambiguous_link(oid, task));
    }
    Ok(Some(BoardReply::new(
        "local",
        BoardResult::Change(BoardChange {
            entry: EntryId::new(sqlite_u64(entry)?).map_err(BoardError::from)?,
            seq: EventSeq::new(sqlite_u64(seq)?),
            plan: Some(task.plan),
            revision: None,
            task: Some(task),
            deduplicated: true,
        }),
    )))
}

fn ambiguous_link(oid: GitOid, task: TaskId) -> BoardError {
    invalid(
        "invalid_reference",
        format!("ambiguous commit link {oid} to {task} across repositories"),
    )
}
