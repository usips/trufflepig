//! Complete-response fitting includes metadata and omission notices.

use super::{RenderedBoard, cell, footer, lines_result};
use crate::board::board_ids::{EventSeq, PlanId};
use crate::board::board_protocol::{BoardReply, BoardResult};
use crate::output::{OutputBudget, OutputFormat};
use anyhow::{Result, bail};
use serde::Serialize;
use std::fmt::Write;

#[derive(Clone, Copy, Default, Serialize)]
pub(super) struct BoardOmitted {
    pub(super) events: usize,
    pub(super) open_entries: usize,
    pub(super) entries: usize,
    pub(super) plans: usize,
    pub(super) feedback: usize,
    pub(super) body_lines: usize,
}

#[derive(Serialize)]
struct RenderEnvelope<'a> {
    #[serde(flatten)]
    reply: &'a BoardReply,
    omitted: BoardOmitted,
    rendered_through: Option<EventSeq>,
    next: Option<String>,
    commit_trailer: Option<String>,
}

pub(super) fn render_list(
    reply: &BoardReply,
    max: usize,
    budget: &OutputBudget,
    result: impl Fn(usize) -> BoardResult,
    omitted: impl Fn(usize) -> BoardOmitted,
) -> Result<RenderedBoard> {
    let render = |count| {
        let mut candidate = reply.clone();
        candidate.result = result(count);
        render_complete(&candidate, omitted(count), None, None, budget)
    };
    let count = fit_items(max, budget, render)?;
    if count == 0 && max != 0 {
        bail!("budget_too_small: no board item fits; raise -b");
    }
    Ok(RenderedBoard {
        text: require_fits(render(count)?, budget)?,
        acknowledge_seq: None,
    })
}

pub(super) fn render_complete(
    reply: &BoardReply,
    omitted: BoardOmitted,
    rendered: Option<EventSeq>,
    next: Option<String>,
    budget: &OutputBudget,
) -> Result<String> {
    let plan = reply_plan(reply);
    if budget.format == OutputFormat::Json {
        return budget.encode(&RenderEnvelope {
            reply,
            omitted,
            rendered_through: rendered,
            next,
            commit_trailer: plan.map(|plan| format!("Plan: {plan}")),
        });
    }
    let mut text = lines_result(&reply.result);
    if matches!(reply.result, BoardResult::Inbox(_)) {
        writeln!(
            text,
            "rendered through: {}",
            rendered.map_or_else(|| "-".into(), |seq| seq.to_string())
        )?;
    }
    for warning in &reply.warnings {
        writeln!(text, "warning: {}", cell(warning))?;
    }
    writeln!(
        text,
        "omitted: events={} open_entries={} entries={} plans={} feedback={} body_lines={}",
        omitted.events,
        omitted.open_entries,
        omitted.entries,
        omitted.plans,
        omitted.feedback,
        omitted.body_lines
    )?;
    if let Some(next) = next {
        writeln!(text, "next: {next}")?;
    }
    footer(&mut text, &reply.backend, plan);
    Ok(text)
}

pub(super) fn reply_plan(reply: &BoardReply) -> Option<PlanId> {
    match &reply.result {
        BoardResult::Plan(view) => Some(view.plan.id),
        BoardResult::Entry(view) => view.entry.plan,
        BoardResult::Revision(revision) => Some(revision.id.plan),
        BoardResult::Diff(diff) => Some(diff.before.id.plan),
        BoardResult::Change(change) => change.plan,
        BoardResult::Review(evidence) => Some(evidence.plan.id),
        BoardResult::Registered(registration) => registration.plan_id,
        _ => None,
    }
}

pub(super) fn committed_receipt(result: &BoardResult) -> Option<String> {
    Some(match result {
        BoardResult::Change(change) => format!(
            "{} seq={} plan={} revision={} task={} deduplicated={}",
            change.entry,
            change.seq,
            change
                .plan
                .map_or_else(|| "-".into(), |plan| plan.to_string()),
            change
                .revision
                .map_or_else(|| "-".into(), |revision| revision.to_string()),
            change
                .task
                .map_or_else(|| "-".into(), |task| task.to_string()),
            change.deduplicated
        ),
        BoardResult::Session(_) => "session registered".into(),
        BoardResult::Cursor(cursor) => format!("cursor {cursor}"),
        BoardResult::Registered(repository) => format!("registered {}", repository.repo_key),
        BoardResult::CommitsLinked(result) => format!("ingest inserted={}", result.inserted),
        BoardResult::Queued { .. } => "queued for import".into(),
        BoardResult::ScanRecorded => "scan recorded".into(),
        BoardResult::RepoPathForgotten => "repository path removed".into(),
        _ => return None,
    })
}

/// Largest complete prefix which fits, including its omission notice and footer.
pub fn fit_items(
    max: usize,
    budget: &OutputBudget,
    mut render: impl FnMut(usize) -> Result<String>,
) -> Result<usize> {
    let (mut low, mut high) = (0, max);
    while low < high {
        let count = low + (high - low).div_ceil(2);
        if budget.fits(&render(count)?) {
            low = count;
        } else {
            high = count - 1;
        }
    }
    Ok(low)
}

pub(super) fn require_fits(text: String, budget: &OutputBudget) -> Result<String> {
    if !budget.fits(&text) {
        bail!(
            "budget_too_small: complete board response exceeds {} o200k_base tokens",
            budget.limit
        );
    }
    Ok(text)
}
