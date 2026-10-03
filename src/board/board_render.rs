//! Complete-response board rendering, with prefix-only inbox acknowledgement.

mod render_budget;
mod render_diff;
mod render_entry;
mod render_inbox;
mod render_lines;
mod render_review;
#[cfg(test)]
mod tests;

pub use render_budget::fit_items;
pub use render_review::render_review;

use render_budget::{
    BoardOmitted, committed_receipt, render_collections, render_complete, reply_plan, require_fits,
};
use render_diff::render_diff;
use render_entry::{render_entry, render_plan};
use render_inbox::render_inbox;
use render_lines::{cell, claim_line, diff_lines, entry_line, footer, lines_result, recipient};

use super::board_ids::EventSeq;
use super::board_protocol::{BOARD_API, BoardReply, BoardResult};
use super::review_packet::assemble_review;
use crate::output::{OutputBudget, OutputFormat};
use anyhow::{Result, bail};

#[derive(Debug)]
pub struct RenderedBoard {
    pub text: String,
    pub acknowledge_seq: Option<EventSeq>,
}

pub fn render_reply(reply: &BoardReply, budget: &OutputBudget) -> Result<RenderedBoard> {
    if reply.api != BOARD_API {
        bail!(
            "board_api_mismatch: expected {BOARD_API}, received {}",
            reply.api
        );
    }
    match &reply.result {
        BoardResult::Inbox(inbox) => render_inbox(reply, inbox, budget),
        BoardResult::Entry(view) => render_entry(reply, view, budget),
        BoardResult::Plan(view) => render_plan(reply, view, budget),
        BoardResult::Overview(_)
        | BoardResult::Attention(_)
        | BoardResult::Feed(_)
        | BoardResult::History(_)
        | BoardResult::Entries(_)
        | BoardResult::Tasks(_)
        | BoardResult::Claims(_)
        | BoardResult::Feedback(_) => render_collections(reply, budget),
        BoardResult::Review(evidence) => render_review(
            &assemble_review(
                evidence,
                evidence.agent.as_ref(),
                &[],
                Vec::new(),
                reply.warnings.clone(),
            ),
            budget,
            &reply.backend,
        ),
        BoardResult::Diff(diff) => render_diff(reply, diff, budget),
        _ => {
            let text = render_complete(reply, BoardOmitted::default(), None, None, budget)?;
            let text = if budget.fits(&text) {
                text
            } else if let Some(receipt) = committed_receipt(&reply.result) {
                let hint = format!(
                    "mutation committed; do not repeat; inspect board show{} -b 1500",
                    reply_plan(reply).map_or_else(String::new, |plan| format!(" {plan}"))
                );
                if budget.format == OutputFormat::Json {
                    let result = if matches!(reply.result, BoardResult::Change(_)) {
                        Some(&reply.result)
                    } else {
                        None
                    };
                    budget.encode(&serde_json::json!({ "api": BOARD_API, "committed": true,
                        "result": result, "receipt": receipt, "warnings_omitted": reply.warnings.len(), "hint": hint }))?
                } else {
                    format!("committed: {receipt}\nhint: {hint}\n")
                }
            } else {
                require_fits(text, budget)?
            };
            Ok(RenderedBoard {
                text,
                acknowledge_seq: None,
            })
        }
    }
}
