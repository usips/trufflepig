//! Immutable plan revisions, proposals, and owner or steward decisions.

#[cfg(test)]
mod tests;

use rusqlite::{OptionalExtension, Transaction, params};

use super::super::*;

mod plan_decisions;
mod plan_revision_storage;

use crate::board::board_vocabulary::{EntryText, PlanText, PlanTitle};
pub(in crate::board::local_board) use plan_decisions::{accept, reject};
use plan_revision_storage::{
    advance_head, insert_revision, proposal_data, require_authority, require_base, store_text,
};

pub(in crate::board::local_board) fn new_plan(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    title: &PlanTitle,
    body: &PlanText,
    steward: Option<&HarnessLabel>,
    repo_key: Option<&RepoKey>,
) -> Result<BoardReply, BoardError> {
    if let Some(repo_key) = repo_key {
        let registered: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM repos WHERE repo_key=?1)",
                [repo_key.as_str()],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        if !registered {
            return Err(invalid(
                "invalid_reference",
                format!("unknown repository {}", repo_key.as_str()),
            ));
        }
    }
    let id: u64 = tx
        .query_row(
            "INSERT INTO plans(title,owner_user,steward,head_revision,created_at) VALUES(?1,?2,?3,1,?4) RETURNING id",
            params![
                title.as_str(),
                ctx.actor.user,
                steward.map(HarnessLabel::as_str),
                ctx.now
            ],
            |r| row_number(r, 0),
        )
        .map_err(sql_error)?;
    let plan = PlanId::new(id).map_err(BoardError::from)?;
    if let Some(repo_key) = repo_key {
        tx.execute(
            "INSERT INTO plan_repos(plan_id,repo_key) VALUES(?1,?2)",
            params![sql_number(plan.get()), repo_key.as_str()],
        )
        .map_err(sql_error)?;
    }
    let entry = mutation_entry(tx, ctx, plan, EntryKind::Create, title.as_str(), None, None)?;
    insert_revision(tx, ctx, plan, 1, body, RevisionSource::Create, entry)?;
    insert_event(
        tx,
        ctx,
        Some(plan),
        EntryKind::Create,
        &entry.to_string(),
        None,
        title.as_str(),
    )?;
    Ok(ctx.change_reply(
        entry,
        Some(plan),
        Some(PlanRevision { plan, revision: 1 }),
        None,
    ))
}

pub(in crate::board::local_board) fn propose(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    base: crate::board::board_ids::PlanRevision,
    body: &PlanText,
    summary: &EntryText,
    supersedes: Option<EntryId>,
) -> Result<BoardReply, BoardError> {
    require_base(tx, base)?;
    if let Some(previous) = supersedes {
        require_superseded_proposal(tx, ctx, base.plan, previous)?;
    }
    let hash = store_text(tx, body)?;
    let entry = mutation_entry(
        tx,
        ctx,
        base.plan,
        EntryKind::Proposal,
        summary.as_str(),
        Some("open"),
        supersedes,
    )?;
    tx.execute(
        "INSERT INTO proposals(entry_id,plan_id,base_revision,text_hash,state) VALUES(?1,?2,?3,?4,'open')",
        params![
            sql_number(entry.get()),
            sql_number(base.plan.get()),
            sql_number(base.revision),
            hash
        ],
    )
    .map_err(sql_error)?;
    if let Some(previous) = supersedes {
        tx.execute(
            "UPDATE proposals SET state='superseded' WHERE entry_id=?1",
            [sql_number(previous.get())],
        )
        .map_err(sql_error)?;
        tx.execute(
            "UPDATE entries SET state='superseded' WHERE id=?1",
            [sql_number(previous.get())],
        )
        .map_err(sql_error)?;
    }
    insert_event(
        tx,
        ctx,
        Some(base.plan),
        EntryKind::Proposal,
        &entry.to_string(),
        None,
        summary.as_str(),
    )?;
    Ok(ctx.change_reply(entry, Some(base.plan), None, None))
}

pub(in crate::board::local_board) fn edit(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    base: crate::board::board_ids::PlanRevision,
    body: &PlanText,
    summary: &EntryText,
) -> Result<BoardReply, BoardError> {
    require_authority(tx, ctx, base.plan)?;
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
    insert_revision(
        tx,
        ctx,
        base.plan,
        number,
        body,
        RevisionSource::Direct,
        entry,
    )?;
    insert_event(
        tx,
        ctx,
        Some(base.plan),
        EntryKind::Direct,
        &entry.to_string(),
        None,
        summary.as_str(),
    )?;
    Ok(ctx.change_reply(
        entry,
        Some(base.plan),
        Some(PlanRevision {
            plan: base.plan,
            revision: number,
        }),
        None,
    ))
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
        },
    )
}

fn require_superseded_proposal(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    plan: PlanId,
    previous: EntryId,
) -> Result<(), BoardError> {
    let prior: Option<(u64, String, String, String)> = tx
        .query_row(
            concat!(
                "SELECT p.plan_id,p.state,a.user,a.harness FROM proposals p JOIN entries e ON e.id=p.entry_id ",
                "JOIN actors a ON a.id=e.actor_id WHERE p.entry_id=?1"
            ),
            [sql_number(previous.get())],
            |row| Ok((row_number(row, 0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(sql_error)?;
    let (prior_plan, state, author_user, author_harness) = prior
        .ok_or_else(|| invalid("invalid_reference", format!("unknown proposal {previous}")))?;
    if prior_plan != plan.get() {
        return Err(invalid(
            "invalid_reference",
            "superseded proposal belongs to a different plan",
        ));
    }
    if author_user != ctx.actor.user || author_harness != ctx.actor.harness.as_str() {
        return Err(invalid(
            "invalid_actor",
            "supersede requires the same user and harness as the proposal author",
        ));
    }
    if state != "open" {
        return Err(invalid(
            "invalid_state",
            format!("proposal {previous} is {state}"),
        ));
    }
    Ok(())
}
