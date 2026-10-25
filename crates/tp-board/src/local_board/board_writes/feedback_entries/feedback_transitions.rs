//! Feedback authority and terminal triage decisions.

use super::*;
use crate::board::board_protocol::BoardErrorCode;

pub(in crate::board::local_board) fn close_feedback(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    op: &BoardOp,
) -> Result<BoardReply, BoardError> {
    let (entry, state, note) = match op {
        BoardOp::FeedbackClose { entry, state, note } => (entry, *state, note),
        BoardOp::FeedbackTriage { entry, note } => (entry, FeedbackState::Triaged, note),
        _ => {
            return Err(invalid(
                BoardErrorCode::InvalidOptions,
                "expected feedback triage or close operation",
            ));
        }
    };
    op.validate().map_err(BoardError::from)?;
    let report = read_entry(tx, *entry)?;
    if report.kind != EntryKind::Feedback {
        return Err(invalid(
            BoardErrorCode::InvalidReference,
            format!("{entry} is not feedback"),
        ));
    }
    let authority = feedback_authority(tx, &ctx.actor, &report, ctx.now)?.ok_or_else(|| {
        invalid(
            BoardErrorCode::InvalidActor,
            "feedback triage requires its exact reporter or triage authority",
        )
    })?;
    let authority_ctx = ctx.with_authority(authority);
    let current: String = tx
        .query_row(
            "SELECT state FROM entries WHERE id=?1",
            [sqlite_id(entry.get())?],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let current = FeedbackState::parse(&current).map_err(BoardError::from)?;
    if current.is_closed() {
        return Err(invalid(
            BoardErrorCode::InvalidState,
            format!("{entry} is already closed {current}"),
        ));
    }
    if state == FeedbackState::Triaged && current == FeedbackState::Triaged {
        return Err(invalid(
            BoardErrorCode::InvalidState,
            format!("{entry} is already triaged"),
        ));
    }
    let mut summary = if state == FeedbackState::Triaged {
        "triaged".to_owned()
    } else {
        format!("closed {state}")
    };
    if let Some(note) = note {
        summary.push_str(&format!(" ({})", note.as_str()));
    }
    truncate_entry_summary(&mut summary);
    let recipient = BoardRecipient::for_actor(&report.actor);
    let closure = insert_entry(
        tx,
        &authority_ctx,
        &EntryDraft {
            plan_id: report.plan,
            kind: EntryKind::Decision,
            body: note
                .as_ref()
                .map_or_else(|| summary.clone(), |note| note.as_str().to_owned()),
            to_whom: Some(recipient.clone()),
            supersedes: Some(*entry),
            repo_key: report.repo_key,
            state: None,
        },
    )?;
    tx.execute(
        "UPDATE entries SET state=?1 WHERE id=?2",
        params![state.as_str(), sqlite_id(entry.get())?],
    )
    .map_err(sql_error)?;
    insert_event(
        tx,
        &authority_ctx,
        report.plan,
        EntryKind::Feedback,
        &entry.to_string(),
        Some(&recipient),
        &summary,
    )?;
    Ok(authority_ctx.change_reply(closure, report.plan, None, None))
}
