//! Bounded review packets retain recent evidence and explicit drill references.

mod review_packet_text;

use super::{RenderedBoard, fit_items};
use crate::board::review_packet::ReviewPacket;
use crate::output::OutputBudget;
use anyhow::{Result, bail};
use review_packet_text::review_text;
use serde::Serialize;

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
