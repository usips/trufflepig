//! Manual link authority and relink dedupe decisions.

use super::*;

/// A plan's owner links by hand or through its steward harness; every other
/// actor is rejected.
pub(in crate::board::local_board) fn require_link_authority(
    tx: &Transaction<'_>,
    actor: &BoardActor,
    plan: PlanId,
) -> Result<(), BoardError> {
    let (owner, steward): (String, Option<String>) = tx
        .query_row(
            "SELECT owner_user,steward FROM plans WHERE id=?1",
            [sql_number(plan.get())],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| invalid("invalid_reference", format!("unknown plan {plan}")))?;
    if actor.user == owner
        && (actor.harness.is_human() || steward.as_deref() == Some(actor.harness.as_str()))
    {
        return Ok(());
    }
    Err(invalid(
        "invalid_actor",
        format!("linking a commit to {plan} requires its owner by hand or its steward"),
    ))
}

/// The shared plan-level commit entry and its original entry sequence.
pub(in crate::board::local_board) struct PlanCommitEntry {
    pub(in crate::board::local_board) entry: EntryId,
    pub(in crate::board::local_board) seq: EventSeq,
}

/// Existing scan links do not have task-specific manual event receipts.
pub(in crate::board::local_board) enum TaskLinkReceipt {
    Manual(EventSeq),
    ManualWithoutEvent,
    Scan,
}

/// The entry associated with an existing plan-level commit link, if any.
pub(in crate::board::local_board) fn plan_commit_entry(
    tx: &Transaction<'_>,
    commit: &LinkedCommit,
    plan: PlanId,
) -> Result<Option<PlanCommitEntry>, BoardError> {
    tx.query_row(
        concat!(
            "SELECT p.entry_id,e.seq FROM commit_plans p JOIN entries e ON e.id=p.entry_id ",
            "WHERE p.repo_key=?1 AND p.oid=?2 AND p.plan_id=?3"
        ),
        params![
            commit.repo_key.as_str(),
            commit.oid.as_str(),
            sql_number(plan.get())
        ],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(sql_error)?
    .map(|(entry, seq): (i64, i64)| {
        Ok(PlanCommitEntry {
            entry: EntryId::new(sqlite_u64(entry)?).map_err(BoardError::from)?,
            seq: EventSeq::new(sqlite_u64(seq)?),
        })
    })
    .transpose()
}

/// The durable receipt for an existing commit-to-task association.
pub(in crate::board::local_board) fn task_link_receipt(
    tx: &Transaction<'_>,
    commit: &LinkedCommit,
    task: TaskId,
) -> Result<Option<TaskLinkReceipt>, BoardError> {
    let row: Option<(String, Option<i64>)> = tx
        .query_row(
            concat!(
                "SELECT source,link_seq FROM commit_tasks ",
                "WHERE repo_key=?1 AND oid=?2 AND plan_id=?3 AND task_ordinal=?4"
            ),
            params![
                commit.repo_key.as_str(),
                commit.oid.as_str(),
                sql_number(task.plan.get()),
                sql_number(task.ordinal)
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sql_error)?;
    row.map(|(source, seq)| match (source.as_str(), seq) {
        ("manual", Some(seq)) => Ok(TaskLinkReceipt::Manual(EventSeq::new(sqlite_u64(seq)?))),
        ("manual", None) => Ok(TaskLinkReceipt::ManualWithoutEvent),
        ("scan", _) => Ok(TaskLinkReceipt::Scan),
        _ => Err(invalid(
            "invalid_state",
            format!("commit link for {task} has unknown source {source:?}"),
        )),
    })
    .transpose()
}

/// Creates the commit entry and `manual` plan link for a first-time link.
pub(in crate::board::local_board) fn record_plan_link(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    commit: &LinkedCommit,
    plan: PlanId,
) -> Result<PlanCommitEntry, BoardError> {
    let body = bounded_summary(&format!("{} {}", commit.oid, commit.subject));
    let entry = insert_entry(
        tx,
        ctx,
        &EntryDraft {
            plan_id: Some(plan),
            kind: EntryKind::Commit,
            body,
            to_whom: None,
            supersedes: None,
            repo_key: Some(commit.repo_key.clone()),
            state: None,
        },
    )?;
    tx.execute(
        "INSERT INTO commit_plans(repo_key,oid,plan_id,entry_id,source) VALUES(?1,?2,?3,?4,'manual')",
        params![
            commit.repo_key.as_str(),
            commit.oid.as_str(),
            sql_number(plan.get()),
            sql_number(entry.get())
        ],
    )
    .map_err(sql_error)?;
    Ok(PlanCommitEntry {
        entry,
        seq: ctx.seq,
    })
}
