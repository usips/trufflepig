//! Owner and steward acceptance or rejection of proposals.

use super::*;

pub(in crate::board::local_board) fn accept(
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
    let event_summary =
        decision_summary("accepted", proposal, &author, note.map(EntryText::as_str));
    let entry = mutation_entry(
        tx,
        ctx,
        plan,
        EntryKind::Accept,
        &summary,
        None,
        Some(proposal),
    )?;
    insert_revision(
        tx,
        ctx,
        plan,
        base.revision + 1,
        &body,
        RevisionSource::Accept,
        entry,
    )?;
    tx.execute(
        "UPDATE proposals SET state='accepted',decision_entry=?1,result_revision=?2 WHERE entry_id=?3",
        params![
            sql_number(entry.get()),
            sql_number(base.revision + 1),
            sql_number(proposal.get())
        ],
    )
    .map_err(sql_error)?;
    tx.execute(
        "UPDATE entries SET state='accepted' WHERE id=?1",
        [sql_number(proposal.get())],
    )
    .map_err(sql_error)?;
    insert_event(
        tx,
        ctx,
        Some(plan),
        EntryKind::Accept,
        &entry.to_string(),
        None,
        &event_summary,
    )?;
    Ok(ctx.change_reply(
        entry,
        Some(plan),
        Some(PlanRevision {
            plan,
            revision: base.revision + 1,
        }),
        None,
    ))
}

pub(in crate::board::local_board) fn reject(
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
    let event_summary = decision_summary("rejected", proposal, &author, Some(reason.as_str()));
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
        EntryKind::Reject,
        &entry.to_string(),
        None,
        &event_summary,
    )?;
    Ok(ctx.change_reply(entry, Some(plan), None, None))
}

fn decision_summary(action: &str, proposal: EntryId, author: &str, detail: Option<&str>) -> String {
    let prefix = format!("{action} {proposal} by {author}");
    let Some(detail) = detail else {
        return prefix;
    };
    let limit = crate::board::board_vocabulary::ENTRY_TEXT_LIMIT;
    let available = limit.saturating_sub(prefix.len() + 2);
    let truncated = detail.len() > available;
    let mut end = detail
        .len()
        .min(available.saturating_sub(usize::from(truncated) * 3));
    while !detail.is_char_boundary(end) {
        end -= 1;
    }
    let mut summary = String::with_capacity(prefix.len() + 2 + end + usize::from(truncated) * 3);
    summary.push_str(&prefix);
    summary.push_str(": ");
    summary.push_str(&detail[..end]);
    if truncated {
        summary.push_str("...");
    }
    summary
}
