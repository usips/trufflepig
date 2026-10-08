//! Prefix fitting preserves each collection's continuation and membership boundary.
use super::{BoardOmitted, fit_items, render_complete, require_fits};
use crate::board::board_protocol::{BoardReply, BoardResult, EntryCursor, EntryRecord};
use crate::board::board_render::RenderedBoard;
use crate::board::board_render::render_scope_hints::cli_scope_flag;
use crate::output::OutputBudget;
use anyhow::{Result, bail};

pub(in crate::board::board_render) fn render_collections(
    reply: &BoardReply,
    budget: &OutputBudget,
    project: Option<&str>,
) -> Result<RenderedBoard> {
    let maximum = match &reply.result {
        BoardResult::Overview(page) => page.plans.len(),
        BoardResult::Attention(page) => page.entries.len(),
        BoardResult::Feed(page) => page.events.len(),
        BoardResult::History(page) => page.revisions.len(),
        BoardResult::Entries(page) => page.entries.len(),
        BoardResult::Tasks(page) => page.tasks.len(),
        BoardResult::Claims(page) => page.claims.len(),
        BoardResult::Feedback(page) => page.feedback.len(),
        _ => unreachable!("collection renderer requires a collection"),
    };
    let with_nested = if matches!(reply.result, BoardResult::Overview(_)) && maximum > 0 {
        let (candidate, omitted) = prefix(reply, 1, 0, true);
        budget.fits(&render_complete(
            &candidate,
            omitted,
            None,
            next_hint(&candidate.result, project),
            budget,
        )?)
    } else {
        true
    };
    let encode = |count, claims| {
        let (candidate, omitted) = prefix(reply, count, claims, with_nested);
        render_complete(
            &candidate,
            omitted,
            None,
            next_hint(&candidate.result, project),
            budget,
        )
    };
    let count = fit_items(maximum, budget, |count| encode(count, 0))?;
    if maximum > 0 && count == 0 {
        bail!("budget_too_small: first board item does not fit; raise -b");
    }
    let claims = if let BoardResult::Attention(page) = &reply.result {
        fit_items(page.stale_claims.len(), budget, |claims| {
            encode(count, claims)
        })?
    } else {
        0
    };
    Ok(RenderedBoard {
        text: require_fits(encode(count, claims)?, budget)?,
        acknowledge_seq: None,
    })
}

fn prefix(
    reply: &BoardReply,
    count: usize,
    claims: usize,
    with_nested: bool,
) -> (BoardReply, BoardOmitted) {
    let mut visible = reply.clone();
    let mut omitted = BoardOmitted::default();
    match &mut visible.result {
        BoardResult::Overview(page) => {
            omitted.plans = page.plans.len() - count;
            if omitted.plans > 0 {
                page.next_after = page
                    .plans
                    .get(count.wrapping_sub(1))
                    .map(|plan| plan.plan.id);
            }
            page.plans.truncate(count);
            page.omitted += omitted.plans;
            // Detail pages carry the nested task and claim records separately.
            for plan in page.plans.iter_mut().filter(|_| !with_nested) {
                plan.tasks_omitted += plan.tasks.len();
                plan.claims_omitted += plan.claims.len();
                plan.tasks.clear();
                plan.recent_done.clear();
                plan.claims.clear();
            }
        }
        BoardResult::Attention(page) => {
            omitted.entries = page.entries.len() - count;
            if omitted.entries > 0 {
                page.next_after = entry_cursor(&page.entries[..count]);
            }
            page.entries.truncate(count);
            page.entries_omitted += omitted.entries;
            page.rebase_needed
                .retain(|id| page.entries.iter().any(|entry| entry.id == *id));
            if claims < page.stale_claims.len() {
                page.claims_next_after = page
                    .stale_claims
                    .get(claims.wrapping_sub(1))
                    .map(|item| item.cursor);
                page.claims_omitted += page.stale_claims.len() - claims;
                page.stale_claims.truncate(claims);
            }
        }
        BoardResult::Feed(page) => {
            omitted.events = page.events.len() - count;
            if omitted.events > 0 {
                page.next_after = page
                    .events
                    .get(count.wrapping_sub(1))
                    .map(|event| event.seq)
                    .or(Some(page.after));
            }
            page.events.truncate(count);
        }
        BoardResult::History(page) => {
            omitted.entries = page.revisions.len() - count;
            if omitted.entries > 0 {
                page.next_after = page
                    .revisions
                    .get(count.wrapping_sub(1))
                    .map(|revision| revision.seq)
                    .or(Some(page.after));
            }
            page.revisions.truncate(count);
        }
        BoardResult::Entries(page) => {
            omitted.entries = page.entries.len() - count;
            if omitted.entries > 0 {
                // Ascending pages carry the request's after cursor; every
                // other page is a descending newest-first window.
                if page.after.is_some() {
                    page.next_after = entry_cursor(&page.entries[..count]).or(page.after);
                } else {
                    page.next_before = entry_cursor(&page.entries[..count]).or(page.next_before);
                }
            }
            page.entries.truncate(count);
        }
        BoardResult::Tasks(page) => {
            omitted.entries = page.tasks.len() - count;
            if omitted.entries > 0 {
                page.next_after = page
                    .tasks
                    .get(count.wrapping_sub(1))
                    .map(|task| task.id)
                    .or(page.after);
            }
            page.tasks.truncate(count);
            page.omitted += omitted.entries;
        }
        BoardResult::Claims(page) => {
            omitted.entries = page.claims.len() - count;
            if omitted.entries > 0 {
                page.next_after = page
                    .claims
                    .get(count.wrapping_sub(1))
                    .map(|claim| claim.cursor)
                    .or(page.after);
            }
            page.claims.truncate(count);
            page.omitted += omitted.entries;
        }
        BoardResult::Feedback(page) => {
            omitted.feedback = page.feedback.len() - count;
            if omitted.feedback > 0 {
                page.next_after = page
                    .feedback
                    .get(count.wrapping_sub(1))
                    .map(|report| EntryCursor {
                        seq: report.entry.seq,
                        entry: report.entry.id,
                    })
                    .or(page.after);
            }
            page.feedback.truncate(count);
            page.omitted += omitted.feedback;
        }
        _ => unreachable!("collection renderer requires a collection"),
    }
    (visible, omitted)
}

fn entry_cursor(entries: &[EntryRecord]) -> Option<EntryCursor> {
    entries.last().map(|entry| EntryCursor {
        seq: entry.seq,
        entry: entry.id,
    })
}

fn next_hint(result: &BoardResult, project: Option<&str>) -> Option<String> {
    match result {
        BoardResult::Overview(page) => page.next_after.map(|after| {
            format!(
                "board show --after {after} --through {}{}",
                page.through,
                cli_scope_flag(&page.scope, project)
            )
        }),
        BoardResult::Feed(page) => page.next_after.map(|after| {
            format!(
                "board feed{} --after {after} --through {}",
                page.plan
                    .map_or_else(String::new, |plan| format!(" {plan}")),
                page.through
            )
        }),
        BoardResult::History(page) => page.next_after.map(|after| {
            format!(
                "board history {} --after {after} --through {}",
                page.plan, page.through
            )
        }),
        BoardResult::Attention(page) => page.next_after.map(|after| {
            format!(
                "board attention --after {}:{} --through {}{}",
                after.seq,
                after.entry,
                page.through,
                cli_scope_flag(&page.scope, project)
            )
        }),
        BoardResult::Feedback(page) => page.next_after.map(|after| {
            format!(
                "feedback ls{} --after {}:{} --through {}",
                if page.open_only { " --open" } else { "" },
                after.seq,
                after.entry,
                page.through
            )
        }),
        _ => None,
    }
}
