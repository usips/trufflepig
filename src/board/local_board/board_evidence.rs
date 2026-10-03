//! Typed entry and event persistence with plan authority checks.

use super::*;

pub(in crate::board) fn insert_entry(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    entry: &EntryDraft,
) -> Result<EntryId, BoardError> {
    crate::board::board_vocabulary::EntryText::new(entry.body.clone()).map_err(BoardError::from)?;
    let id: u64 = tx.query_row("INSERT INTO entries(plan_id,kind,body,to_whom,supersedes,actor_id,model,effort,repo_key,state,seq,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12) RETURNING id", params![entry.plan_id.map(|id|sql_number(id.get())), entry.kind.as_str(), entry.body, entry.to_whom.as_ref().map(BoardRecipient::as_str), entry.supersedes.map(|id|sql_number(id.get())), ctx.actor_id, ctx.model, ctx.effort, entry.repo_key.as_ref().map(RepoKey::as_str), entry.state, sql_number(ctx.seq.get()), ctx.now], |r| row_number(r,0)).map_err(sql_error)?;
    let id = EntryId::new(id).map_err(BoardError::from)?;
    for reference in extract_refs(&entry.body)
        .into_iter()
        .chain(entry.supersedes.map(BoardRef::Entry))
    {
        tx.execute(
            "INSERT OR IGNORE INTO entry_refs(entry_id,target) VALUES(?1,?2)",
            params![sql_number(id.get()), reference.to_string()],
        )
        .map_err(sql_error)?;
    }
    Ok(id)
}

pub(in crate::board) fn insert_event(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    plan: Option<PlanId>,
    kind: EntryKind,
    subject: &str,
    to: Option<&BoardRecipient>,
    summary: &str,
) -> Result<(), BoardError> {
    crate::board::board_vocabulary::EntryText::new(summary.to_owned()).map_err(BoardError::from)?;
    tx.execute("INSERT INTO events(seq,plan_id,kind,subject,to_whom,actor_id,summary,created_at,model,effort) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![sql_number(ctx.seq.get()), plan.map(|id|sql_number(id.get())), kind.as_str(), subject, to.map(BoardRecipient::as_str), ctx.actor_id, summary, ctx.now, ctx.model, ctx.effort]).map_err(sql_error)?;
    Ok(())
}

pub(in crate::board) fn can_accept(
    conn: &Connection,
    actor: &BoardActor,
    plan: PlanId,
) -> Result<bool, BoardError> {
    let (owner, steward): (String, Option<String>) = conn
        .query_row(
            "SELECT owner_user,steward FROM plans WHERE id=?1",
            [sql_number(plan.get())],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| invalid("invalid_reference", format!("unknown plan {plan}")))?;
    Ok(actor.user == owner
        && (actor.harness.is_human() || steward.as_deref() == Some(actor.harness.as_str())))
}

pub(in crate::board) fn require_plan(conn: &Connection, plan: PlanId) -> Result<(), BoardError> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM plans WHERE id=?1)",
            [sql_number(plan.get())],
            |r| r.get(0),
        )
        .map_err(sql_error)?;
    if exists {
        Ok(())
    } else {
        Err(invalid("invalid_reference", format!("unknown plan {plan}")))
    }
}
