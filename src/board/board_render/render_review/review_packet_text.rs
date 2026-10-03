//! Review envelopes summarize proposals and disclose omitted evidence.

use super::super::{cell, claim_line, diff_lines, entry_line, footer, recipient};
use super::ReviewRenderOmitted;
use crate::board::{board_protocol::BOARD_API, review_packet::ReviewPacket};
use crate::output::{OutputBudget, OutputFormat};
use anyhow::Result;
use std::fmt::Write;

pub(super) fn review_text(
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
