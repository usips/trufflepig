//! Pending notices remain read-only and keep their generation with the page.

use super::{BoardOmitted, RenderedBoard, fit_items, render_complete, require_fits};
use crate::board_protocol::{BoardReply, BoardResult, PendingReply};
use anyhow::Result;
use tp_core::output::OutputBudget;

pub(super) fn render_pending(
    reply: &BoardReply,
    pending: &PendingReply,
    budget: &OutputBudget,
) -> Result<RenderedBoard> {
    let render = |count: usize| {
        let mut visible = pending.clone();
        let omitted = visible.notices.len() - count;
        visible.notices.truncate(count);
        visible.omitted += omitted;
        let next = (visible.omitted > 0).then(|| next_hint(&visible)).flatten();
        let mut candidate = reply.clone();
        candidate.result = BoardResult::Pending(visible);
        render_complete(
            &candidate,
            BoardOmitted {
                notices: omitted,
                ..BoardOmitted::default()
            },
            None,
            next,
            budget,
        )
    };
    let count = fit_items(pending.notices.len(), budget, render)?;
    Ok(RenderedBoard {
        text: require_fits(render(count)?, budget)?,
        acknowledge_seq: None,
    })
}

/// Resumes after the newest visible notice past the request cursor. Open reminders sort after
/// newer notices, so a page with none past the cursor has nothing newer to resume from.
fn next_hint(visible: &PendingReply) -> Option<String> {
    visible
        .notices
        .iter()
        .map(|notice| notice.seq)
        .filter(|seq| visible.after.is_none_or(|after| *seq > after))
        .max()
        .map(|seq| format!("board pending {seq}"))
}
