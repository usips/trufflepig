//! Pending notices remain read-only and keep their generation with the page.

use super::{BoardOmitted, RenderedBoard, fit_items, render_complete, require_fits};
use crate::board::board_protocol::{BoardReply, BoardResult, PendingReply};
use crate::output::OutputBudget;
use anyhow::Result;

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
        let next = (omitted > 0)
            .then(|| {
                visible
                    .notices
                    .last()
                    .map(|notice| format!("board pending {}", notice.seq))
            })
            .flatten();
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
