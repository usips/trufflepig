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

/// The receipt of a commit's existing plan link, when the plan has one.
pub(in crate::board::local_board) fn plan_link_receipt(
    tx: &Transaction<'_>,
    commit: &LinkedCommit,
    plan: PlanId,
) -> Result<Option<(EntryId, EventSeq)>, BoardError> {
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
        Ok((
            EntryId::new(sqlite_u64(entry)?).map_err(BoardError::from)?,
            EventSeq::new(sqlite_u64(seq)?),
        ))
    })
    .transpose()
}

/// Creates the commit entry and `manual` plan link for a first-time link.
pub(in crate::board::local_board) fn record_plan_link(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    commit: &LinkedCommit,
    plan: PlanId,
) -> Result<EntryId, BoardError> {
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
    Ok(entry)
}
