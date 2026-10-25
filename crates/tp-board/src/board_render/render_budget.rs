//! Complete-response fitting includes metadata and omission notices.

mod collection_budget;

pub(super) use collection_budget::render_collections;

use super::{cell, footer, lines_result};
use crate::board_ids::{EventSeq, PlanId};
use crate::board_protocol::{BoardReply, BoardResult};
use anyhow::{Result, bail};
use serde::Serialize;
use std::fmt::Write;
use tp_core::output::{OutputBudget, OutputFormat};

#[derive(Clone, Copy, Default, Serialize)]
pub(super) struct BoardOmitted {
    pub(super) events: usize,
    pub(super) open_entries: usize,
    pub(super) entries: usize,
    pub(super) plans: usize,
    pub(super) feedback: usize,
    pub(super) body_lines: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(super) claims: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(super) grants: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(super) commits: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(super) commit_worker_refs: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(super) commit_claim_links: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(super) notices: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(super) workers: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(super) worker_files: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(super) progress_gates: usize,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
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
    if omitted.claims > 0
        || omitted.grants > 0
        || omitted.commits > 0
        || omitted.commit_worker_refs > 0
        || omitted.commit_claim_links > 0
        || omitted.notices > 0
        || omitted.workers > 0
        || omitted.worker_files > 0
        || omitted.progress_gates > 0
    {
        writeln!(
            text,
            "nested omitted: claims={} grants={} commits={} commit_worker_refs={} commit_claim_links={} notices={} workers={} worker_files={} progress_gates={}",
            omitted.claims,
            omitted.grants,
            omitted.commits,
            omitted.commit_worker_refs,
            omitted.commit_claim_links,
            omitted.notices,
            omitted.workers,
            omitted.worker_files,
            omitted.progress_gates
        )?;
    }
    if let Some(next) = next {
        writeln!(text, "next: {next}")?;
    }
    footer(&mut text, &reply.backend, plan);
    Ok(text)
}

pub(super) fn reply_plan(reply: &BoardReply) -> Option<PlanId> {
    match &reply.result {
        BoardResult::Plan(view) => Some(view.plan.id),
        BoardResult::TaskView(view) => Some(view.task.id.plan),
        BoardResult::Entry(view) => view.entry.plan,
        BoardResult::Revision(revision) => Some(revision.id.plan),
        BoardResult::Diff(diff) => Some(diff.before.id.plan),
        BoardResult::Change(change) => change.plan,
        BoardResult::Receipt(receipt) => receipt.change.as_ref().and_then(|change| change.plan),
        BoardResult::Review(evidence) => Some(evidence.plan.id),
        BoardResult::History(page) => Some(page.plan),
        BoardResult::Tasks(page) => Some(page.plan),
        BoardResult::Feed(page) => page.plan,
        BoardResult::Entries(page) => page.plan,
        BoardResult::Claims(page) => page.plan,
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
        BoardResult::Receipt(receipt)
            if receipt.state == crate::board_protocol::BoardReceiptState::Committed =>
        {
            let Some(change) = &receipt.change else {
                return None;
            };
            format!(
                "request_key={} {} seq={}",
                receipt.key, change.entry, change.seq
            )
        }
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
        bail!(coded!(
            BudgetTooSmall,
            "complete board response exceeds {} o200k_base tokens",
            budget.limit
        ));
    }
    Ok(text)
}
