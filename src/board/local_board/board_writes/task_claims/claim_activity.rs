//! Lease activity updates from board and matching commit evidence.

use super::*;

pub(in crate::board::local_board) fn refresh_plan_claims(
    tx: &Transaction<'_>,
    actor_id: i64,
    plan: PlanId,
    now: i64,
) -> Result<(), BoardError> {
    tx.execute(
        "UPDATE claims SET last_active=max(last_active,?3) WHERE actor_id=?1 AND plan_id=?2 AND ended_at IS NULL",
        params![actor_id, sql_number(plan.get()), now],
    ).map_err(sql_error)?;
    Ok(())
}

pub(in crate::board::local_board) fn refresh_inbox_claims(
    tx: &Transaction<'_>,
    actor_id: i64,
    now: i64,
) -> Result<(), BoardError> {
    tx.execute(
        "UPDATE claims SET last_active=max(last_active,?2) WHERE actor_id=?1 AND ended_at IS NULL",
        params![actor_id, now],
    )
    .map_err(sql_error)?;
    Ok(())
}

pub(in crate::board::local_board) fn refresh_commit_claims(
    tx: &Transaction<'_>,
    plan: PlanId,
    ordinal: u64,
    coauthors: &[CommitCoauthor],
    committed_at: i64,
    now: i64,
) -> Result<(), BoardError> {
    if committed_at > now {
        return Ok(());
    }
    let task = TaskId::new(plan, ordinal).map_err(BoardError::from)?;
    let Some(holder) = active_claim(tx, task, now, 0)? else {
        return Ok(());
    };
    let vendor = claim_vendor(&holder.record.actor.harness, holder.record.model.as_deref());
    let matches = if coauthors.is_empty() {
        vendor.is_human()
    } else {
        coauthors.iter().any(|coauthor| coauthor.harness == vendor)
    };
    if matches && committed_at >= holder.record.claimed_at {
        tx.execute(
            "UPDATE claims SET last_active=max(last_active,?3) WHERE entry_id=?1 AND actor_id=?2 AND ended_at IS NULL",
            params![sql_number(holder.record.entry.get()), holder.actor_id, committed_at],
        ).map_err(sql_error)?;
    }
    Ok(())
}
