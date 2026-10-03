//! Complete-response board rendering, with prefix-only inbox acknowledgement.

mod review_render;
#[cfg(test)]
mod tests;

pub use review_render::render_review;

use super::board_ids::{EventSeq, PlanId};
use super::board_protocol::*;
use super::board_vocabulary::{EntryKind, ProposalState};
use super::review_packet::{SsotDiff, assemble_review, build_ssot_diff};
use crate::output::{OutputBudget, OutputFormat};
use anyhow::{Result, bail};
use serde::Serialize;
use std::fmt::Write;

#[derive(Debug)]
pub struct RenderedBoard {
    pub text: String,
    pub rendered_seq: Option<EventSeq>,
}

#[derive(Clone, Copy, Default, Serialize)]
struct BoardOmitted {
    events: usize,
    open_entries: usize,
    entries: usize,
    plans: usize,
    feedback: usize,
    body_lines: usize,
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
        BoardResult::Plans(plans) => render_list(
            reply,
            plans.len(),
            budget,
            |count| BoardResult::Plans(plans[..count].to_vec()),
            |count| BoardOmitted {
                plans: plans.len() - count,
                ..BoardOmitted::default()
            },
        ),
        BoardResult::Feedback(feedback) => render_list(
            reply,
            feedback.len(),
            budget,
            |count| BoardResult::Feedback(feedback[..count].to_vec()),
            |count| BoardOmitted {
                feedback: feedback.len() - count,
                ..BoardOmitted::default()
            },
        ),
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
                rendered_seq: None,
            })
        }
    }
}

fn committed_receipt(result: &BoardResult) -> Option<String> {
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
        _ => return None,
    })
}

fn render_list(
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
        rendered_seq: None,
    })
}

fn render_inbox(
    reply: &BoardReply,
    inbox: &InboxReply,
    budget: &OutputBudget,
) -> Result<RenderedBoard> {
    if inbox
        .events
        .windows(2)
        .any(|events| events[0].seq >= events[1].seq)
    {
        bail!("board_unavailable: inbox events are not strictly ordered");
    }
    let render = |count: usize, open_count: usize| {
        let mut visible = inbox.clone();
        visible.events.truncate(count);
        visible.open.truncate(open_count);
        let rendered = visible.events.last().map(|event| event.seq);
        let next = if count < inbox.events.len() {
            format!("board inbox {}", rendered.unwrap_or(inbox.cursor))
        } else {
            "board inbox --wait".to_owned()
        };
        let mut candidate = reply.clone();
        candidate.result = BoardResult::Inbox(visible);
        if open_count < inbox.open.len() {
            let mut hints = inbox.open[open_count..]
                .iter()
                .filter_map(|entry| entry.plan)
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .take(5)
                .map(|plan| format!("board show {plan}"))
                .collect::<Vec<_>>();
            hints.push("board show".into());
            if inbox.open[open_count..]
                .iter()
                .any(|entry| entry.kind == EntryKind::Feedback)
            {
                hints.push("feedback ls".into());
            }
            candidate.warnings.push(format!(
                "open evidence omitted; inspect {}",
                hints.join(", ")
            ));
        }
        render_complete(
            &candidate,
            BoardOmitted {
                events: inbox.events.len() - count,
                open_entries: inbox.open.len() - open_count,
                ..BoardOmitted::default()
            },
            rendered,
            Some(next),
            budget,
        )
    };
    let open_count = fit_items(inbox.open.len(), budget, |open_count| {
        render(inbox.events.len().min(1), open_count)
    })?;
    let count = fit_items(inbox.events.len(), budget, |count| {
        render(count, open_count)
    })?;
    if count == 0 && !inbox.events.is_empty() {
        bail!("budget_too_small: first inbox event and open evidence do not fit; raise -b");
    }
    Ok(RenderedBoard {
        text: require_fits(render(count, open_count)?, budget)?,
        rendered_seq: if inbox.advancing && count > 0 {
            Some(inbox.events[count - 1].seq)
        } else {
            None
        },
    })
}

fn render_entry(
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
            proposal.body =
                super::board_vocabulary::PlanText::new(body_lines[..body_count].concat())?;
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
        rendered_seq: None,
    })
}

fn render_plan(
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
        visible.revision.body =
            super::board_vocabulary::PlanText::new(body_lines[..lines].concat())?;
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
            rendered_seq: None,
        });
    }
    let lines = fit_items(body_lines.len(), budget, |lines| render(0, lines))?;
    Ok(RenderedBoard {
        text: require_fits(render(0, lines)?, budget)?,
        rendered_seq: None,
    })
}

fn is_open(entry: &EntryRecord) -> bool {
    entry.kind == EntryKind::Question
        || matches!(entry.state, Some(EntryState::Proposal(ProposalState::Open)))
        || matches!(entry.state, Some(EntryState::Feedback(state)) if !state.is_closed())
}

fn render_complete(
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

fn reply_plan(reply: &BoardReply) -> Option<PlanId> {
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

fn lines_result(result: &BoardResult) -> String {
    let mut text = String::new();
    match result {
        BoardResult::Inbox(inbox) => {
            let first = inbox.events.first().map_or(inbox.cursor, |event| event.seq);
            let last = inbox.events.last().map_or(inbox.cursor, |event| event.seq);
            let plans = inbox
                .events
                .iter()
                .filter_map(|event| event.plan)
                .collect::<std::collections::BTreeSet<_>>()
                .len();
            writeln!(
                text,
                "inbox {} {first}..{last} ({} events, {plans} plans) wait={}",
                cell(&inbox.actor.identity()),
                inbox.events.len(),
                match inbox.wait {
                    InboxWait::None => "none",
                    InboxWait::Ready => "ready",
                    InboxWait::Timeout => "timeout",
                    InboxWait::Busy => "busy",
                }
            )
            .unwrap();
            for event in &inbox.events {
                writeln!(
                    text,
                    "{}\t{}\t{}\t{}\t{}\t{}{}",
                    event.seq,
                    event
                        .plan
                        .map_or_else(|| "-".to_owned(), |plan| plan.to_string()),
                    event.subject,
                    event.kind,
                    author(
                        &event.actor,
                        event.model.as_deref(),
                        event.effort.as_deref()
                    ),
                    cell(event.summary.as_str()),
                    recipient(&event.to)
                )
                .unwrap();
            }
            if !inbox.open.is_empty() {
                text.push_str("--- still open ---\n");
            }
            for entry in &inbox.open {
                entry_line(&mut text, entry);
            }
        }
        BoardResult::Entry(view) => {
            entry_line(&mut text, &view.entry);
            writeln!(
                text,
                "permissions: decide={} supersede={}",
                view.can_decide, view.can_supersede
            )
            .unwrap();
            if let Some(proposal) = &view.proposal {
                writeln!(
                    text,
                    "proposal {} base={}@{} state={}",
                    proposal.entry, proposal.plan, proposal.base_revision, proposal.state
                )
                .unwrap();
                for line in proposal.body.as_str().split_inclusive('\n') {
                    writeln!(text, "| {}", cell(line.trim_end_matches('\n'))).unwrap();
                }
            }
            for entry in &view.replies {
                entry_line(&mut text, entry);
            }
            for entry in &view.backrefs {
                entry_line(&mut text, entry);
            }
            writeln!(
                text,
                "omitted: replies={} backrefs={}",
                view.replies_omitted, view.backrefs_omitted
            )
            .unwrap();
        }
        BoardResult::Plan(view) => plan_lines(&mut text, view),
        BoardResult::Plans(plans) => {
            text.push_str("plans\n");
            for plan in plans {
                writeln!(
                    text,
                    "{}@{}\t{}\towner={} steward={}",
                    plan.id,
                    plan.head_revision,
                    cell(plan.title.as_str()),
                    cell(&plan.owner_user),
                    plan.steward.as_ref().map_or("-", |h| h.as_str())
                )
                .unwrap();
            }
        }
        BoardResult::Revision(revision) => revision_lines(&mut text, revision),
        BoardResult::Session(session) => writeln!(
            text,
            "hello {} model={} effort={} cursor={}",
            cell(&session.actor.identity()),
            cell(session.model.as_deref().unwrap_or("unclaimed")),
            cell(session.effort.as_deref().unwrap_or("unclaimed")),
            session.cursor
        )
        .unwrap(),
        BoardResult::Cursor(seq) => writeln!(text, "cursor {seq}").unwrap(),
        BoardResult::Change(change) => writeln!(
            text,
            "{} seq={} plan={} revision={} task={} deduplicated={}",
            change.entry,
            change.seq,
            change.plan.map_or_else(|| "-".into(), |v| v.to_string()),
            change
                .revision
                .map_or_else(|| "-".into(), |v| v.to_string()),
            change.task.map_or_else(|| "-".into(), |v| v.to_string()),
            change.deduplicated
        )
        .unwrap(),
        BoardResult::Feedback(feedback) => {
            text.push_str("feedback\n");
            for report in feedback {
                writeln!(
                    text,
                    "{}\t{}\t{}",
                    report.entry.id, report.kind, report.state
                )
                .unwrap();
                entry_line(&mut text, &report.entry);
            }
        }
        BoardResult::Repositories(repositories) => {
            for repository in repositories {
                writeln!(
                    text,
                    "repo {}\t{}\t{}",
                    repository.registration.repo_key,
                    cell(&repository.registration.common_dir.to_string_lossy()),
                    cell(repository.scan_error.as_deref().unwrap_or("ready"))
                )
                .unwrap();
            }
        }
        BoardResult::Registered(repository) => writeln!(
            text,
            "registered {}\t{}",
            repository.repo_key,
            cell(&repository.common_dir.to_string_lossy())
        )
        .unwrap(),
        BoardResult::CommitsLinked(result) => {
            writeln!(text, "ingest inserted={}", result.inserted).unwrap();
            for plan in &result.unknown_plans {
                writeln!(text, "unknown plan: {plan}").unwrap();
            }
            for task in &result.unknown_tasks {
                writeln!(text, "unknown task: {task}; commit linked to its plan").unwrap();
            }
        }
        BoardResult::Queued { import_key } => writeln!(
            text,
            "queued: pending import ({})",
            cell(&import_key.to_string())
        )
        .unwrap(),
        BoardResult::ScanRecorded => text.push_str("scan recorded\n"),
        BoardResult::Diff(_) | BoardResult::Review(_) => {
            unreachable!("diffs and reviews have dedicated renderers")
        }
    }
    text
}

fn plan_lines(text: &mut String, view: &PlanView) {
    text.push_str("--- working now ---\n");
    for claim in view.claims.iter().filter(|claim| claim.ended_at.is_none()) {
        claim_line(text, claim);
    }
    text.push_str("--- open for claiming ---\n");
    for task in &view.tasks {
        if view
            .claims
            .iter()
            .any(|claim| claim.task == task.id && claim.ended_at.is_none() && !claim.stale)
        {
            continue;
        }
        if matches!(
            task.column,
            super::board_vocabulary::TaskColumn::Done | super::board_vocabulary::TaskColumn::Review
        ) {
            continue;
        }
        writeln!(
            text,
            "{}\t{}\t{}{}",
            task.id,
            task.column,
            cell(task.title.as_str()),
            task.section
                .as_ref()
                .map_or_else(String::new, |section| format!("\t§ {}", cell(section)))
        )
        .unwrap();
    }
    text.push_str("--- plan sections without tasks ---\n");
    for section in &view.sections_without_tasks {
        writeln!(text, "§ {}", cell(section)).unwrap();
    }
    writeln!(
        text,
        "--- {} {} ---",
        view.revision.id,
        cell(view.plan.title.as_str())
    )
    .unwrap();
    revision_lines(text, &view.revision);
    text.push_str("--- recent evidence ---\n");
    for entry in &view.entries {
        entry_line(text, entry);
    }
}

fn revision_lines(text: &mut String, revision: &RevisionRecord) {
    writeln!(
        text,
        "revision {} source={:?} seq={}",
        revision.id, revision.source, revision.seq
    )
    .unwrap();
    // A fixed prefix keeps SSOT lines distinct from controlled grammar/footer lines.
    for line in revision.body.as_str().split_inclusive('\n') {
        writeln!(text, "| {}", cell(line.strip_suffix('\n').unwrap_or(line))).unwrap();
    }
}

fn entry_line(text: &mut String, entry: &EntryRecord) {
    writeln!(
        text,
        "{}\t{}\t{}\t{}{}",
        entry.id,
        entry.kind,
        author(
            &entry.actor,
            entry.model.as_deref(),
            entry.effort.as_deref()
        ),
        cell(entry.body.as_str()),
        recipient(&entry.to)
    )
    .unwrap();
}

fn claim_line(text: &mut String, claim: &ClaimRecord) {
    writeln!(
        text,
        "{}\t{}\tsince={} active={}\t{}\t{}",
        claim.task,
        author(
            &claim.actor,
            claim.model.as_deref(),
            claim.effort.as_deref()
        ),
        claim.claimed_at,
        claim.last_active,
        if claim.stale {
            "STALE (claimable)"
        } else if claim.ended_at.is_some() {
            "ended"
        } else {
            "active"
        },
        cell(claim.scope.as_str())
    )
    .unwrap();
}

fn author(
    actor: &super::board_actor::BoardActor,
    model: Option<&str>,
    effort: Option<&str>,
) -> String {
    format!(
        "{}({}/{})",
        cell(&actor.identity()),
        cell(model.unwrap_or("unclaimed")),
        cell(effort.unwrap_or("unclaimed"))
    )
}

fn recipient(to: &Option<super::board_actor::BoardRecipient>) -> String {
    to.as_ref()
        .map_or_else(String::new, |to| format!(" (to {})", cell(to.as_str())))
}

fn footer(text: &mut String, backend: &str, plan: Option<PlanId>) {
    writeln!(text, "backend: {}", cell(backend)).unwrap();
    if let Some(plan) = plan {
        writeln!(text, "commit trailer: Plan: {plan}").unwrap();
    }
}

fn diff_lines(text: &mut String, diff: &SsotDiff) {
    for hunk in &diff.hunks {
        let context = hunk.context_before.len() + hunk.context_after.len();
        let before_count = hunk.removed.len() + context;
        let after_count = hunk.added.len() + context;
        let before_start =
            hunk.before_start - hunk.context_before.len() - usize::from(before_count == 0);
        let after_start =
            hunk.after_start - hunk.context_before.len() - usize::from(after_count == 0);
        writeln!(
            text,
            "@@ -{},{} +{},{} @@",
            before_start, before_count, after_start, after_count
        )
        .unwrap();
        for (prefix, lines) in [
            (" ", &hunk.context_before),
            ("-", &hunk.removed),
            ("+", &hunk.added),
            (" ", &hunk.context_after),
        ] {
            for line in lines {
                writeln!(
                    text,
                    "{prefix}{}",
                    cell(line.strip_suffix('\n').unwrap_or(line))
                )
                .unwrap();
                if !line.ends_with('\n') {
                    text.push_str("\\ No newline at end of file\n");
                }
            }
        }
    }
    if let Some(next) = &diff.next {
        writeln!(text, "next: {next}").unwrap();
    }
}

fn render_diff(
    reply: &BoardReply,
    diff: &RevisionDiff,
    budget: &OutputBudget,
) -> Result<RenderedBoard> {
    let mut ssot = build_ssot_diff(&diff.before, &diff.after);
    let render = |ssot: &SsotDiff, omitted: BoardOmitted| -> Result<String> {
        if budget.format == OutputFormat::Json {
            return budget.encode(&serde_json::json!({ "api": reply.api, "backend": reply.backend,
                "result": { "result": "diff", "data": ssot }, "omitted": omitted, "warnings": reply.warnings,
                "commit_trailer": format!("Plan: {}", diff.before.id.plan) }));
        }
        let mut text = format!("diff {}..{}\n", diff.before.id, diff.after.id);
        diff_lines(&mut text, ssot);
        for warning in &reply.warnings {
            writeln!(text, "warning: {}", cell(warning))?;
        }
        writeln!(text, "omitted: body_lines={}", omitted.body_lines)?;
        footer(&mut text, &reply.backend, Some(diff.before.id.plan));
        Ok(text)
    };
    let full = render(&ssot, BoardOmitted::default())?;
    if budget.fits(&full) {
        return Ok(RenderedBoard {
            text: full,
            rendered_seq: None,
        });
    }
    let omitted = ssot
        .hunks
        .iter()
        .map(|h| h.removed.len() + h.added.len() + h.context_before.len() + h.context_after.len())
        .sum();
    ssot.hunks.clear();
    ssot.next = Some(format!(
        "board show {}@{}..{} -b 32768",
        diff.before.id.plan, diff.before.id.revision, diff.after.id.revision
    ));
    Ok(RenderedBoard {
        text: require_fits(
            render(
                &ssot,
                BoardOmitted {
                    body_lines: omitted,
                    ..BoardOmitted::default()
                },
            )?,
            budget,
        )?,
        rendered_seq: None,
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

fn require_fits(text: String, budget: &OutputBudget) -> Result<String> {
    if !budget.fits(&text) {
        bail!(
            "budget_too_small: complete board response exceeds {} o200k_base tokens",
            budget.limit
        );
    }
    Ok(text)
}

/// Keep user-authored text inside one controlled line, including terminal escapes.
fn cell(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\\' => escaped.push_str("\\\\"),
            ch if ch.is_control() => {
                use std::fmt::Write;
                write!(escaped, "\\u{{{:x}}}", u32::from(ch))
                    .expect("writing a string cannot fail");
            }
            ch => escaped.push(ch),
        }
    }
    escaped
}
