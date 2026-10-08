//! Inbox rendering fits fresh events before reminders and acknowledges prefixes.

use super::{BoardOmitted, RenderedBoard, fit_items, render_complete, require_fits};
use crate::board::board_protocol::{BoardReply, BoardResult, InboxReply};
use crate::board::board_vocabulary::EntryKind;
use crate::output::OutputBudget;
use anyhow::{Result, bail};

pub(super) fn render_inbox(
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
    if inbox
        .events
        .last()
        .is_some_and(|event| event.seq > inbox.scanned_through)
    {
        bail!("board_unavailable: inbox scan watermark precedes selected events");
    }
    let render = |count: usize, open_count: usize| {
        let mut visible = inbox.clone();
        visible.events.truncate(count);
        visible.open.truncate(open_count);
        let rendered = visible.events.last().map(|event| event.seq);
        let scope_flag = if inbox.scope.is_all() { " --all" } else { "" };
        let next = if count < inbox.events.len() || inbox.query_truncated {
            if inbox.advancing {
                format!("board inbox{scope_flag}")
            } else {
                format!(
                    "board inbox {}{scope_flag}",
                    rendered.unwrap_or(inbox.cursor)
                )
            }
        } else {
            format!("board inbox --wait{scope_flag}")
        };
        let mut candidate = reply.clone();
        candidate.result = BoardResult::Inbox(visible);
        let omitted_open = inbox
            .open_omitted
            .saturating_add(inbox.open.len() - open_count);
        if omitted_open > 0 {
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
                || inbox.open_omitted > 0
            {
                hints.push("feedback ls".into());
            }
            let bound = if inbox.open_omitted_lower_bound {
                " (lower bound)"
            } else {
                ""
            };
            candidate.warnings.push(format!(
                "open evidence omitted{bound}; inspect {}",
                hints.join(", ")
            ));
        }
        render_complete(
            &candidate,
            BoardOmitted {
                events: inbox.events.len() - count,
                open_entries: omitted_open,
                ..BoardOmitted::default()
            },
            rendered,
            Some(next),
            budget,
        )
    };
    let count = fit_items(inbox.events.len(), budget, |count| render(count, 0))?;
    if count == 0 && !inbox.events.is_empty() {
        bail!("budget_too_small: first inbox event does not fit; raise -b");
    }
    let open_count = fit_items(inbox.open.len(), budget, |open_count| {
        render(count, open_count)
    })?;
    Ok(RenderedBoard {
        text: require_fits(render(count, open_count)?, budget)?,
        acknowledge_seq: if !inbox.advancing {
            None
        } else if count < inbox.events.len() || inbox.query_truncated {
            (count > 0).then(|| inbox.events[count - 1].seq)
        } else {
            (inbox.scanned_through > inbox.cursor).then_some(inbox.scanned_through)
        },
    })
}
