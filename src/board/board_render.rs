//! Complete-response board rendering, with prefix-only inbox acknowledgement.

#[cfg(test)]
mod tests;

use super::board_ids::{EventSeq, PlanId};
use super::board_protocol::*;
use super::board_vocabulary::{EntryKind, ProposalState};
use super::review_packet::{ReviewPacket, SsotDiff, assemble_review, build_ssot_diff};
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
        _ => Ok(RenderedBoard {
            text: require_fits(
                render_complete(reply, BoardOmitted::default(), None, None, budget)?,
                budget,
            )?,
            rendered_seq: None,
        }),
    }
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
                "inbox {} {first}..{last} ({} events, {plans} plans) wait={:?}",
                cell(&inbox.actor.identity()),
                inbox.events.len(),
                inbox.wait
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
        }
        BoardResult::Queued { import_key } => {
            writeln!(text, "queued: pending import ({})", cell(&import_key.to_string())).unwrap()
        }
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

pub fn render_review(
    packet: &ReviewPacket,
    budget: &OutputBudget,
    backend: &str,
) -> Result<RenderedBoard> {
    let render = |packet: &ReviewPacket| review_text(packet, budget, backend);
    let mut visible = packet.clone();
    let full = render(&visible)?;
    if budget.fits(&full) {
        return Ok(RenderedBoard {
            text: full,
            rendered_seq: None,
        });
    }
    let total = visible.entries.len();
    let kept = fit_items(total, budget, |kept| {
        let mut candidate = visible.clone();
        candidate.entries = visible.entries[total - kept..].to_vec();
        candidate.omitted.entries += total - kept;
        render(&candidate)
    })?;
    visible.entries.drain(..total - kept);
    visible.omitted.entries += total - kept;
    let text = render(&visible)?;
    if budget.fits(&text) {
        return Ok(RenderedBoard {
            text,
            rendered_seq: None,
        });
    }
    visible.trim_file_stats();
    let text = render(&visible)?;
    if budget.fits(&text) {
        return Ok(RenderedBoard {
            text,
            rendered_seq: None,
        });
    }
    visible.trim_diff_context();
    let text = render(&visible)?;
    if budget.fits(&text) {
        return Ok(RenderedBoard {
            text,
            rendered_seq: None,
        });
    }
    visible.trim_diff_body();
    Ok(RenderedBoard {
        text: require_fits(render(&visible)?, budget)?,
        rendered_seq: None,
    })
}

fn review_text(packet: &ReviewPacket, budget: &OutputBudget, backend: &str) -> Result<String> {
    if budget.format == OutputFormat::Json {
        return budget.encode(&serde_json::json!({
            "api": BOARD_API, "backend": backend, "result": {"result": "review", "data": packet},
            "commit_trailer": format!("Plan: {}", packet.plan.id),
        }));
    }
    let mut text = format!(
        "review {}..{} agent={}\n--- SSOT diff ---\n",
        packet.base,
        packet.head,
        packet.agent.as_ref().map_or("all", |agent| agent.as_str())
    );
    diff_lines(&mut text, &packet.ssot_diff);
    text.push_str("--- entries ---\n");
    for entry in &packet.entries {
        entry_line(&mut text, entry);
    }
    text.push_str("--- tasks ---\n");
    for task in &packet.tasks {
        writeln!(
            text,
            "{}\t{}\t{}{}",
            task.id,
            task.column,
            cell(task.title.as_str()),
            recipient(&task.assignee)
        )?;
    }
    text.push_str("--- claims during window ---\n");
    for claim in &packet.claims {
        claim_line(&mut text, claim);
    }
    for (title, commits) in [("linked", &packet.linked), ("unlinked", &packet.unlinked)] {
        writeln!(text, "--- {title} commits ---")?;
        for commit in commits {
            writeln!(
                text,
                "{}\t{}\tfiles={} +{} -{}",
                commit.commit.oid,
                cell(&commit.commit.subject),
                commit.commit.files,
                commit.commit.insertions,
                commit.commit.deletions
            )?;
            writeln!(text, "  author: {}", cell(&commit.commit.author))?;
            if commit.commit.coauthors.is_empty() {
                text.push_str("  attribution: human\n");
            } else {
                for coauthor in &commit.commit.coauthors {
                    writeln!(
                        text,
                        "  coauthor: {} ({}) <{}>",
                        cell(coauthor.harness.as_str()),
                        cell(&coauthor.model),
                        cell(&coauthor.email)
                    )?;
                }
            }
            for link in &commit.commit.plans {
                writeln!(
                    text,
                    "  Plan: {}{}",
                    link.plan_id,
                    link.task_ordinal
                        .map_or_else(String::new, |ordinal| format!(
                            " Plan-Task: {}.{ordinal}",
                            link.plan_id
                        ))
                )?;
            }
            for stat in &commit.commit.file_stats {
                writeln!(
                    text,
                    "  {} +{} -{}",
                    cell(&stat.path),
                    stat.insertions
                        .map_or_else(|| "binary".into(), |n| n.to_string()),
                    stat.deletions
                        .map_or_else(|| "binary".into(), |n| n.to_string())
                )?;
            }
            if let Some(drill) = &commit.drill {
                writeln!(text, "drill: {}", cell(drill))?;
            }
        }
    }
    text.push_str("--- crossed commits ---\n");
    for crossed in &packet.crossed {
        writeln!(
            text,
            "{}\t{} claimed by {}\t{}\t{}",
            crossed.oid,
            crossed.task,
            cell(&crossed.claimant.identity()),
            crossed.claim_entry,
            cell(crossed.scope.as_str())
        )?;
    }
    text.push_str("--- open proposals ---\n");
    for proposal in &packet.open_proposals {
        writeln!(
            text,
            "{} base={}@{}",
            proposal.entry, proposal.plan, proposal.base_revision
        )?;
        for line in proposal.body.as_str().split_inclusive('\n') {
            writeln!(text, "| {}", cell(line.trim_end_matches('\n')))?;
        }
    }
    text.push_str("--- open questions ---\n");
    for entry in &packet.open_questions {
        entry_line(&mut text, entry);
    }
    text.push_str("--- open feedback ---\n");
    for feedback in &packet.open_feedback {
        entry_line(&mut text, &feedback.entry);
    }
    for error in &packet.scan_errors {
        writeln!(text, "scan incomplete: {}", cell(error))?;
    }
    writeln!(
        text,
        "omitted: entries={} file_stats={} diff_context_lines={} diff_body_lines={}",
        packet.omitted.entries,
        packet.omitted.file_stats,
        packet.omitted.diff_context_lines,
        packet.omitted.diff_body_lines
    )?;
    footer(&mut text, backend, Some(packet.plan.id));
    Ok(text)
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
