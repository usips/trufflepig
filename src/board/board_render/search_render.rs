//! Search results fit as complete immutable hits with explicit budget omissions.
use super::render_budget::{BoardOmitted, render_complete, require_fits};
use super::{RenderedBoard, fit_items};
use crate::board::board_protocol::{BoardReply, BoardResult, BoardSearchReply};
use crate::output::OutputBudget;
use anyhow::{Result, bail};
#[cfg(test)]
mod tests;

pub(super) fn render(
    reply: &BoardReply,
    result: &BoardSearchReply,
    budget: &OutputBudget,
) -> Result<RenderedBoard> {
    let encode = |count: usize| {
        let mut candidate = reply.clone();
        let mut visible = result.clone();
        let omitted = visible.hits.len() - count;
        visible.hits.truncate(count);
        visible.truncated |= omitted > 0;
        if omitted > 0 {
            candidate.warnings.push(
                "search results omitted to fit output budget; refine the query or raise -b".into(),
            );
        } else if visible.truncated {
            candidate
                .warnings
                .push("search result limit reached; refine the query".into());
        }
        candidate.result = BoardResult::Search(visible);
        render_complete(
            &candidate,
            BoardOmitted {
                entries: omitted,
                ..BoardOmitted::default()
            },
            None,
            None,
            budget,
        )
    };
    let count = fit_items(result.hits.len(), budget, encode)?;
    if count == 0 && !result.hits.is_empty() {
        bail!("budget_too_small: first search hit does not fit; raise -b");
    }
    Ok(RenderedBoard {
        text: require_fits(encode(count)?, budget)?,
        acknowledge_seq: None,
    })
}
