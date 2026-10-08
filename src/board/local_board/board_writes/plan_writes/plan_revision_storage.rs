//! Immutable text storage and compare-and-swap plan revision advances.

use super::*;

pub(super) fn store_text(tx: &Transaction<'_>, body: &PlanText) -> Result<String, BoardError> {
    let hash = blake3::hash(body.as_str().as_bytes()).to_hex().to_string();
    tx.execute(
        "INSERT OR IGNORE INTO texts(hash,body) VALUES(?1,?2)",
        params![hash, body.as_str()],
    )
    .map_err(sql_error)?;
    Ok(hash)
}

pub(super) fn insert_revision(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    plan: PlanId,
    number: u64,
    body: &PlanText,
    source: RevisionSource,
    entry: EntryId,
) -> Result<(), BoardError> {
    let hash = store_text(tx, body)?;
    tx.execute(
        "INSERT INTO revisions(plan_id,number,text_hash,source,entry_id,actor_id,seq) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![
            sql_number(plan.get()),
            sql_number(number),
            hash,
            source.as_str(),
            sql_number(entry.get()),
            ctx.actor_id,
            sql_number(ctx.seq.get())
        ],
    )
    .map_err(sql_error)?;
    Ok(())
}

pub(super) fn require_base(
    tx: &Transaction<'_>,
    base: crate::board::board_ids::PlanRevision,
) -> Result<(), BoardError> {
    require_plan(tx, base.plan)?;
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM revisions WHERE plan_id=?1 AND number=?2)",
            params![sql_number(base.plan.get()), sql_number(base.revision)],
            |r| r.get(0),
        )
        .map_err(sql_error)?;
    if exists {
        Ok(())
    } else {
        Err(invalid(
            "invalid_reference",
            format!("unknown revision {base}"),
        ))
    }
}

pub(super) fn advance_head(
    tx: &Transaction<'_>,
    base: crate::board::board_ids::PlanRevision,
) -> Result<(), BoardError> {
    require_base(tx, base)?;
    let updated = tx
        .execute(
            "UPDATE plans SET head_revision=head_revision+1 WHERE id=?1 AND head_revision=?2",
            params![sql_number(base.plan.get()), sql_number(base.revision)],
        )
        .map_err(sql_error)?;
    if updated == 0 {
        let head: u64 = tx
            .query_row(
                "SELECT head_revision FROM plans WHERE id=?1",
                [sql_number(base.plan.get())],
                |r| row_number(r, 0),
            )
            .map_err(sql_error)?;
        return Err(invalid(
            "stale_revision",
            format!("{base} is stale; head is {}@{head}", base.plan),
        ));
    }
    Ok(())
}

pub(super) fn require_authority(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    plan: PlanId,
) -> Result<(), BoardError> {
    if can_accept(tx, &ctx.actor, plan)? {
        Ok(())
    } else {
        let (owner, steward): (String, Option<String>) = tx
            .query_row(
                "SELECT owner_user,steward FROM plans WHERE id=?1",
                [sql_number(plan.get())],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(sql_error)?;
        let guidance = match steward {
            Some(steward) => format!(
                "ask owner {owner} acting as human or steward {steward} under that user to make this decision"
            ),
            None => format!(
                "no steward is assigned; ask owner {owner} acting as human to make this decision. Creating a plan does not grant approval authority; new plans can explicitly delegate with board new --steward HARNESS TITLE"
            ),
        };
        Err(invalid(
            "invalid_actor",
            format!(
                "{} is not the owner or steward of {plan}; {guidance}",
                ctx.actor
            ),
        ))
    }
}

pub(super) fn proposal_data(
    tx: &Transaction<'_>,
    entry: EntryId,
) -> Result<(PlanId, u64, PlanText, String, String), BoardError> {
    let value: Option<(u64, u64, String, String, BoardActor)> = tx
        .query_row(
            concat!(
                "SELECT p.plan_id,p.base_revision,t.body,p.state,a.user,a.host,a.harness,a.session ",
                "FROM proposals p JOIN texts t ON t.hash=p.text_hash JOIN entries e ON e.id=p.entry_id ",
                "JOIN actors a ON a.id=e.actor_id WHERE p.entry_id=?1"
            ),
            [sql_number(entry.get())],
            |r| {
                Ok((
                    row_number(r, 0)?,
                    row_number(r, 1)?,
                    r.get(2)?,
                    r.get(3)?,
                    actor_from_row(r, 4)?,
                ))
            },
        )
        .optional()
        .map_err(sql_error)?;
    let (plan, base, body, state, actor) =
        value.ok_or_else(|| invalid("invalid_reference", format!("unknown proposal {entry}")))?;
    Ok((
        PlanId::new(plan).map_err(BoardError::from)?,
        base,
        PlanText::new(body).map_err(BoardError::from)?,
        state,
        actor.identity(),
    ))
}
