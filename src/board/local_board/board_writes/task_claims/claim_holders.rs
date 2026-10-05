//! Delegated holder resolution and holder-conflict errors.
use super::*;

pub(super) struct ResolvedDelegate {
    actor_id: i64,
    pub(super) actor: BoardActor,
    model: Option<String>,
    effort: Option<String>,
    pub(super) delegated_by: i64,
}

impl ResolvedDelegate {
    pub(super) fn holder_context(&self, ctx: &WriteContext) -> WriteContext {
        WriteContext {
            actor_id: self.actor_id,
            actor: self.actor.clone(),
            model: self.model.clone(),
            effort: self.effort.clone(),
            now: ctx.now,
            seq: ctx.seq,
            claim_ttl_secs: ctx.claim_ttl_secs,
            via: ctx.via,
        }
    }
}

pub(super) fn resolve_delegate(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    task: TaskId,
    target: &ClaimDelegate,
) -> Result<ResolvedDelegate, BoardError> {
    let holder = target.holder(&ctx.actor).map_err(BoardError::from)?;
    let owner: String = tx
        .query_row(
            "SELECT owner_user FROM plans WHERE id=?1",
            [sql_number(task.plan.get())],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| invalid("invalid_reference", format!("unknown plan {}", task.plan)))?;
    // Standing is the delegator's, never the delegate's: the plan owner's user.
    if ctx.actor.user != owner {
        return Err(invalid(
            "invalid_actor",
            format!("{task} delegation requires the plan owner's user"),
        ));
    }
    let actor_id = ensure_actor_without_seen_bump(tx, &holder, ctx.now)?;
    let (model, effort): (Option<String>, Option<String>) = tx
        .query_row(
            "SELECT model,effort FROM agent_sessions WHERE actor_id=?1",
            [actor_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(sql_error)?;
    Ok(ResolvedDelegate {
        actor_id,
        actor: holder,
        model,
        effort,
        delegated_by: ctx.actor_id,
    })
}

pub(super) fn claim_conflict(task: TaskId, claim: &ClaimRecord, now: i64) -> BoardError {
    invalid(
        "claim_conflict",
        format!(
            "{task} held by {} ({}/{}) since {}, active {}s ago (last activity {})",
            claim.actor,
            claim.model.as_deref().unwrap_or("unknown"),
            claim.effort.as_deref().unwrap_or("unknown"),
            claim.claimed_at,
            now.saturating_sub(claim.last_active),
            claim.last_active,
        ),
    )
}
