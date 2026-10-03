//! Revision diffs retain their recovery hint when the body exceeds the budget.

use super::{BoardOmitted, RenderedBoard, cell, diff_lines, footer, require_fits};
use crate::board::board_protocol::{BoardReply, RevisionDiff};
use crate::board::review_packet::{SsotDiff, build_ssot_diff};
use crate::output::{OutputBudget, OutputFormat};
use anyhow::Result;
use std::fmt::Write;

pub(super) fn render_diff(
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
            acknowledge_seq: None,
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
        acknowledge_seq: None,
    })
}
