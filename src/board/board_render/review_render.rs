//! Bounded review packets retain recent evidence and explicit drill references.

use super::{
    RenderedBoard, cell, claim_line, diff_lines, entry_line, fit_items, footer, recipient,
};
use crate::board::{board_protocol::BOARD_API, review_packet::ReviewPacket};
use crate::output::{OutputBudget, OutputFormat};
use anyhow::{Result, bail};
use serde::Serialize;
use std::fmt::Write;

pub fn render_review(
    packet: &ReviewPacket,
    budget: &OutputBudget,
    backend: &str,
) -> Result<RenderedBoard> {
    let render = |packet: &ReviewPacket, omitted| review_text(packet, omitted, budget, backend);
    let mut visible = packet.clone();
    let mut omitted = ReviewRenderOmitted::default();
    let full = render(&visible, omitted)?;
    if budget.fits(&full) {
        return Ok(RenderedBoard {
            text: full,
            acknowledge_seq: None,
        });
    }
    let refit = |visible: &mut ReviewPacket, omitted| -> Result<String> {
        let total = packet.entries.len();
        let kept = fit_items(total, budget, |kept| {
            let mut candidate = visible.clone();
            candidate.entries = packet.entries[total - kept..].to_vec();
            candidate.omitted.entries = packet.omitted.entries + total - kept;
            render(&candidate, omitted)
        })?;
        visible.entries = packet.entries[total - kept..].to_vec();
        visible.omitted.entries = packet.omitted.entries + total - kept;
        render(visible, omitted)
    };
    for stage in 0..3 {
        match stage {
            1 => visible.trim_diff_context(),
            2 => visible.trim_diff_body(),
            _ => {}
        }
        let text = refit(&mut visible, omitted)?;
        if budget.fits(&text) {
            return Ok(RenderedBoard {
                text,
                acknowledge_seq: None,
            });
        }
    }
    // Keep one record per section before allowing a minimal packet without it.
    for floor in [1, 0] {
        let mut order = (0..visible.linked.len().saturating_sub(floor))
            .map(|index| (true, index))
            .chain((0..visible.unlinked.len().saturating_sub(floor)).map(|index| (false, index)))
            .collect::<Vec<_>>();
        let commit = |linked: bool, index: usize| {
            if linked {
                &visible.linked[index].commit
            } else {
                &visible.unlinked[index].commit
            }
        };
        order.sort_by(|&(a_linked, a), &(b_linked, b)| {
            let (a, b) = (commit(a_linked, a), commit(b_linked, b));
            (a.committed_at, &a.repo_key, &a.oid).cmp(&(b.committed_at, &b.repo_key, &b.oid))
        });
        let trim_commits =
            |candidate: &mut ReviewPacket, omitted: &mut ReviewRenderOmitted, kept| {
                let lost = &order[..order.len() - kept];
                let linked = lost.iter().filter(|(linked, _)| *linked).count();
                omitted.linked_commits += linked;
                omitted.unlinked_commits += lost.len() - linked;
                let mut index = 0;
                candidate.linked.retain(|_| {
                    let keep = !lost.contains(&(true, index));
                    index += 1;
                    keep
                });
                index = 0;
                candidate.unlinked.retain(|_| {
                    let keep = !lost.contains(&(false, index));
                    index += 1;
                    keep
                });
            };
        if !order.is_empty() {
            let kept = fit_items(order.len(), budget, |kept| {
                let mut candidate = visible.clone();
                let mut counts = omitted;
                trim_commits(&mut candidate, &mut counts, kept);
                candidate.entries.clear();
                candidate.omitted.entries = packet.omitted.entries + packet.entries.len();
                render(&candidate, counts)
            })?;
            trim_commits(&mut visible, &mut omitted, kept);
            let text = refit(&mut visible, omitted)?;
            if budget.fits(&text) {
                return Ok(RenderedBoard {
                    text,
                    acknowledge_seq: None,
                });
            }
        }
        macro_rules! trim {
            ($field:ident, $counter:ident) => {{
                let total = visible.$field.len();
                if total > floor {
                    let kept = fit_items(total, budget, |kept| {
                        let mut candidate = visible.clone();
                        candidate.$field.drain(..total - kept);
                        candidate.entries.clear();
                        candidate.omitted.entries = packet.omitted.entries + packet.entries.len();
                        let mut candidate_omitted = omitted;
                        candidate_omitted.$counter += total - kept;
                        render(&candidate, candidate_omitted)
                    })?
                    .max(floor);
                    visible.$field.drain(..total - kept);
                    omitted.$counter += total - kept;
                    let text = refit(&mut visible, omitted)?;
                    if budget.fits(&text) {
                        return Ok(RenderedBoard {
                            text,
                            acknowledge_seq: None,
                        });
                    }
                }
            }};
        }
        trim!(tasks, tasks);
        trim!(claims, claims);
        trim!(crossed, crossed_commits);
        trim!(open_proposals, open_proposals);
        trim!(open_questions, open_questions);
        trim!(open_feedback, open_feedback);
        trim!(scan_errors, scan_errors);
    }
    bail!(
        "budget_too_small: minimal review packet exceeds {} o200k_base tokens; raise -b",
        budget.limit
    )
}

#[derive(Clone, Copy, Default, Serialize)]
struct ReviewRenderOmitted {
    linked_commits: usize,
    unlinked_commits: usize,
    tasks: usize,
    claims: usize,
    crossed_commits: usize,
    open_proposals: usize,
    open_questions: usize,
    open_feedback: usize,
    scan_errors: usize,
}

fn review_text(
    packet: &ReviewPacket,
    omitted: ReviewRenderOmitted,
    budget: &OutputBudget,
    backend: &str,
) -> Result<String> {
    let mut counts = serde_json::to_value(&packet.omitted)?;
    counts
        .as_object_mut()
        .unwrap()
        .extend(serde_json::to_value(omitted)?.as_object().unwrap().clone());
    let next = counts
        .as_object()
        .unwrap()
        .values()
        .any(|n| n.as_u64().unwrap() != 0)
        .then(|| {
            format!(
                "board review {}{} -b {}",
                packet.base,
                packet
                    .agent
                    .as_ref()
                    .map_or_else(String::new, |agent| format!(" {agent}")),
                budget.limit.saturating_mul(2).max(32768)
            )
        });
    if budget.format == OutputFormat::Json {
        let mut data = serde_json::to_value(packet)?;
        data["omitted"] = counts;
        data["next"] = serde_json::to_value(next)?;
        data["open_proposals"] = serde_json::Value::Array(
            packet
                .open_proposals
                .iter()
                .map(|proposal| {
                    let mut summary =
                        serde_json::to_value(proposal).expect("proposal serialization cannot fail");
                    summary.as_object_mut().unwrap().remove("body");
                    summary["stale_base"] = serde_json::json!(proposal.stale_base);
                    summary["body_lines"] =
                        serde_json::json!(proposal.body.as_str().lines().count());
                    summary["drill"] =
                        serde_json::json!(format!("board show {} -b 32768", proposal.entry));
                    summary
                })
                .collect(),
        );
        return budget.encode(&serde_json::json!({
            "api": BOARD_API, "backend": backend, "result": {"result": "review", "data": data},
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
            "{} base=@{} ({} lines; board show {} -b 32768){}",
            proposal.entry,
            proposal.base_revision,
            proposal.body.as_str().lines().count(),
            proposal.entry,
            if proposal.stale_base {
                " stale base; rebase before acceptance"
            } else {
                ""
            }
        )?;
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
    text.push_str("omitted:");
    for (field, count) in counts.as_object().unwrap() {
        write!(text, " {field}={count}")?;
    }
    text.push('\n');
    if let Some(next) = next {
        writeln!(text, "next: {next}")?;
    }
    footer(&mut text, backend, Some(packet.plan.id));
    Ok(text)
}
