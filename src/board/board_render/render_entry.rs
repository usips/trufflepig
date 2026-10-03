//! Entry and plan views preserve protected evidence while fitting optional detail.

use super::{BoardOmitted, RenderedBoard, fit_items, render_complete, require_fits};
use crate::board::board_protocol::{
    BoardReply, BoardResult, EntryRecord, EntryState, EntryView, PlanView,
};
use crate::board::board_vocabulary::{EntryKind, PlanText, ProposalState};
use crate::output::OutputBudget;
use anyhow::Result;

pub(super) fn render_entry(
    reply: &BoardReply,
    view: &EntryView,
    budget: &OutputBudget,
) -> Result<RenderedBoard> {
    let body_lines = view
        .proposal
        .as_ref()
        .map(|proposal| {
            proposal
                .body
                .as_str()
                .split_inclusive('\n')
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let render = |body_count: usize, reply_count: usize, backref_count: usize| -> Result<String> {
        let mut visible = view.clone();
        visible.replies.truncate(reply_count);
        visible.backrefs.truncate(backref_count);
        visible.replies_omitted = view
            .replies_omitted
            .saturating_add(view.replies.len() - reply_count);
        visible.backrefs_omitted = view
            .backrefs_omitted
            .saturating_add(view.backrefs.len() - backref_count);
        if let Some(proposal) = &mut visible.proposal {
            proposal.body = PlanText::new(body_lines[..body_count].concat())?;
        }
        let mut candidate = reply.clone();
        candidate.result = BoardResult::Entry(visible);
        if body_count < body_lines.len() {
            candidate.warnings.push(format!(
                "proposal body omitted; inspect board show {} -b {}",
                view.entry.id,
                budget.limit.saturating_mul(2).max(32768)
            ));
        }
        render_complete(
            &candidate,
            BoardOmitted {
                body_lines: body_lines.len() - body_count,
                entries: view.replies.len() - reply_count + view.backrefs.len() - backref_count,
                ..BoardOmitted::default()
            },
            None,
            None,
            budget,
        )
    };
    let body_count = if budget.fits(&render(body_lines.len(), 0, 0)?) {
        body_lines.len()
    } else {
        fit_items(body_lines.len(), budget, |count| render(count, 0, 0))?
    };
    let reply_count = fit_items(view.replies.len(), budget, |count| {
        render(body_count, count, 0)
    })?;
    let backref_count = fit_items(view.backrefs.len(), budget, |count| {
        render(body_count, reply_count, count)
    })?;
    Ok(RenderedBoard {
        text: require_fits(render(body_count, reply_count, backref_count)?, budget)?,
        acknowledge_seq: None,
    })
}

pub(super) fn render_plan(
    reply: &BoardReply,
    view: &PlanView,
    budget: &OutputBudget,
) -> Result<RenderedBoard> {
    let protected = view
        .entries
        .iter()
        .filter(|entry| is_open(entry))
        .cloned()
        .collect::<Vec<_>>();
    let recent = view
        .entries
        .iter()
        .filter(|entry| !is_open(entry))
        .cloned()
        .collect::<Vec<_>>();
    let body_lines = view
        .revision
        .body
        .as_str()
        .split_inclusive('\n')
        .collect::<Vec<_>>();
    let render = |entries: usize, lines: usize| {
        let mut visible = view.clone();
        visible.entries = protected.clone();
        visible
            .entries
            .extend_from_slice(&recent[recent.len() - entries..]);
        visible.entries.sort_by_key(|entry| entry.seq);
        visible.revision.body = PlanText::new(body_lines[..lines].concat())?;
        let omitted = BoardOmitted {
            entries: recent.len() - entries,
            body_lines: body_lines.len() - lines,
            ..BoardOmitted::default()
        };
        let next =
            (lines < body_lines.len()).then(|| format!("board show {} -b 32768", view.revision.id));
        let mut candidate = reply.clone();
        candidate.result = BoardResult::Plan(visible);
        render_complete(&candidate, omitted, None, next, budget)
    };
    let entries = fit_items(recent.len(), budget, |entries| {
        render(entries, body_lines.len())
    })?;
    let full = render(entries, body_lines.len())?;
    if budget.fits(&full) {
        return Ok(RenderedBoard {
            text: full,
            acknowledge_seq: None,
        });
    }
    let lines = fit_items(body_lines.len(), budget, |lines| render(0, lines))?;
    Ok(RenderedBoard {
        text: require_fits(render(0, lines)?, budget)?,
        acknowledge_seq: None,
    })
}

fn is_open(entry: &EntryRecord) -> bool {
    entry.kind == EntryKind::Question
        || matches!(entry.state, Some(EntryState::Proposal(ProposalState::Open)))
        || matches!(entry.state, Some(EntryState::Feedback(state)) if !state.is_closed())
}
