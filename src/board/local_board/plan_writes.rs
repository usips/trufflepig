//! Immutable plan revisions, proposals, and owner or steward decisions.

use rusqlite::{OptionalExtension, Transaction, params};

use super::*;
use crate::board::board_vocabulary::{EntryText, PlanText, PlanTitle};

pub(super) fn new_plan(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    title: &PlanTitle,
    body: &PlanText,
    steward: Option<&HarnessLabel>,
) -> Result<BoardReply, BoardError> {
    let id: u64 = tx.query_row("INSERT INTO plans(title,owner_user,steward,head_revision,created_at) VALUES(?1,?2,?3,1,?4) RETURNING id", params![title.as_str(),ctx.actor.user,steward.map(HarnessLabel::as_str),ctx.now], |r| row_number(r,0)).map_err(sql_error)?;
    let plan = PlanId::new(id).map_err(BoardError::from)?;
    let entry = mutation_entry(tx, ctx, plan, EntryKind::Create, title.as_str(), None, None)?;
    insert_revision(tx, ctx, plan, 1, body, "create", entry)?;
    insert_event(
        tx,
        ctx,
        Some(plan),
        "create",
        &entry.to_string(),
        None,
        title.as_str(),
    )?;
    Ok(change(ctx, entry, plan, Some(1)))
}

pub(super) fn propose(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    base: crate::board::board_ids::PlanRevision,
    body: &PlanText,
    summary: &EntryText,
) -> Result<BoardReply, BoardError> {
    require_base(tx, base)?;
    let hash = store_text(tx, body)?;
    let entry = mutation_entry(
        tx,
        ctx,
        base.plan,
        EntryKind::Proposal,
        summary.as_str(),
        Some("open"),
        None,
    )?;
    tx.execute("INSERT INTO proposals(entry_id,plan_id,base_revision,text_hash,state) VALUES(?1,?2,?3,?4,'open')", params![sql_number(entry.get()),sql_number(base.plan.get()),sql_number(base.revision),hash]).map_err(sql_error)?;
    insert_event(
        tx,
        ctx,
        Some(base.plan),
        "proposal",
        &entry.to_string(),
        None,
        summary.as_str(),
    )?;
    Ok(change(ctx, entry, base.plan, None))
}

pub(super) fn edit(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    base: crate::board::board_ids::PlanRevision,
    body: &PlanText,
    summary: &EntryText,
) -> Result<BoardReply, BoardError> {
    advance_head(tx, base)?;
    let number = base.revision + 1;
    let entry = mutation_entry(
        tx,
        ctx,
        base.plan,
        EntryKind::Direct,
        summary.as_str(),
        None,
        None,
    )?;
    insert_revision(tx, ctx, base.plan, number, body, "direct", entry)?;
    insert_event(
        tx,
        ctx,
        Some(base.plan),
        "direct",
        &entry.to_string(),
        None,
        summary.as_str(),
    )?;
    Ok(change(ctx, entry, base.plan, Some(number)))
}

pub(super) fn accept(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    proposal: EntryId,
    note: Option<&EntryText>,
) -> Result<BoardReply, BoardError> {
    let (plan, base, body, state, author) = proposal_data(tx, proposal)?;
    require_authority(tx, ctx, plan)?;
    if state != "open" {
        return Err(invalid(
            "invalid_state",
            format!("proposal {proposal} is {state}"),
        ));
    }
    let base = crate::board::board_ids::PlanRevision::new(plan, base).map_err(BoardError::from)?;
    advance_head(tx, base)?;
    let summary = note.map_or_else(|| format!("accepted {proposal}"), |n| n.as_str().to_owned());
    let entry = mutation_entry(
        tx,
        ctx,
        plan,
        EntryKind::Accept,
        &summary,
        None,
        Some(proposal),
    )?;
    insert_revision(tx, ctx, plan, base.revision + 1, &body, "accept", entry)?;
    tx.execute("UPDATE proposals SET state='accepted',decision_entry=?1,result_revision=?2 WHERE entry_id=?3", params![sql_number(entry.get()),sql_number(base.revision+1),sql_number(proposal.get())]).map_err(sql_error)?;
    tx.execute(
        "UPDATE entries SET state='accepted' WHERE id=?1",
        [sql_number(proposal.get())],
    )
    .map_err(sql_error)?;
    insert_event(
        tx,
        ctx,
        Some(plan),
        "accept",
        &entry.to_string(),
        Some(&author),
        &summary,
    )?;
    Ok(change(ctx, entry, plan, Some(base.revision + 1)))
}

pub(super) fn reject(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    proposal: EntryId,
    reason: &EntryText,
) -> Result<BoardReply, BoardError> {
    let (plan, _, _, state, author) = proposal_data(tx, proposal)?;
    require_authority(tx, ctx, plan)?;
    if state != "open" {
        return Err(invalid(
            "invalid_state",
            format!("proposal {proposal} is {state}"),
        ));
    }
    let entry = mutation_entry(
        tx,
        ctx,
        plan,
        EntryKind::Reject,
        reason.as_str(),
        None,
        Some(proposal),
    )?;
    tx.execute(
        "UPDATE proposals SET state='rejected',decision_entry=?1 WHERE entry_id=?2",
        params![sql_number(entry.get()), sql_number(proposal.get())],
    )
    .map_err(sql_error)?;
    tx.execute(
        "UPDATE entries SET state='rejected' WHERE id=?1",
        [sql_number(proposal.get())],
    )
    .map_err(sql_error)?;
    insert_event(
        tx,
        ctx,
        Some(plan),
        "reject",
        &entry.to_string(),
        Some(&author),
        reason.as_str(),
    )?;
    Ok(change(ctx, entry, plan, None))
}

fn mutation_entry(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    plan: PlanId,
    kind: EntryKind,
    body: &str,
    state: Option<&str>,
    supersedes: Option<EntryId>,
) -> Result<EntryId, BoardError> {
    insert_entry(
        tx,
        ctx,
        &EntryDraft {
            plan_id: Some(plan),
            kind,
            body: body.to_owned(),
            to_whom: None,
            supersedes,
            repo_key: None,
            state: state.map(str::to_owned),
            dedupe_key: None,
        },
    )
}

fn store_text(tx: &Transaction<'_>, body: &PlanText) -> Result<String, BoardError> {
    let hash = blake3::hash(body.as_str().as_bytes()).to_hex().to_string();
    tx.execute(
        "INSERT OR IGNORE INTO texts(hash,body) VALUES(?1,?2)",
        params![hash, body.as_str()],
    )
    .map_err(sql_error)?;
    Ok(hash)
}

fn insert_revision(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    plan: PlanId,
    number: u64,
    body: &PlanText,
    source: &str,
    entry: EntryId,
) -> Result<(), BoardError> {
    let hash = store_text(tx, body)?;
    tx.execute("INSERT INTO revisions(plan_id,number,text_hash,source,entry_id,actor_id,seq) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![sql_number(plan.get()),sql_number(number),hash,source,sql_number(entry.get()),ctx.actor_id,sql_number(ctx.seq.get())]).map_err(sql_error)?;
    Ok(())
}

fn require_base(
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

fn advance_head(
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

fn require_authority(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    plan: PlanId,
) -> Result<(), BoardError> {
    if can_accept(tx, &ctx.actor, plan)? {
        Ok(())
    } else {
        Err(invalid(
            "invalid_actor",
            format!("{} is not the owner or steward of {plan}", ctx.actor),
        ))
    }
}

fn proposal_data(
    tx: &Transaction<'_>,
    entry: EntryId,
) -> Result<(PlanId, u64, PlanText, String, String), BoardError> {
    let value:Option<(u64,u64,String,String,BoardActor)> = tx.query_row("SELECT p.plan_id,p.base_revision,t.body,p.state,a.user,a.host,a.harness,a.session FROM proposals p JOIN texts t ON t.hash=p.text_hash JOIN entries e ON e.id=p.entry_id JOIN actors a ON a.id=e.actor_id WHERE p.entry_id=?1",[sql_number(entry.get())],|r|Ok((row_number(r,0)?,row_number(r,1)?,r.get(2)?,r.get(3)?,actor_from_row(r,4)?))).optional().map_err(sql_error)?;
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

fn change(ctx: &WriteContext, entry: EntryId, plan: PlanId, revision: Option<u64>) -> BoardReply {
    BoardReply::new(
        "local",
        BoardResult::Change(BoardChange {
            entry,
            seq: ctx.seq,
            plan: Some(plan),
            revision: revision
                .map(|revision| crate::board::board_ids::PlanRevision { plan, revision }),
            task: None,
            deduplicated: false,
        }),
    )
}
