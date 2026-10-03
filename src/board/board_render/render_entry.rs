//! Plan and entry views preserve primary bodies while fitting related evidence.
use super::{BoardOmitted, RenderedBoard, fit_items, render_complete, require_fits};
use crate::board::board_protocol::{
    BoardReply, BoardResult, EntryCursor, EntryRecord, EntryView, PlanView,
};
use crate::board::board_vocabulary::PlanText;
use crate::output::OutputBudget;
use anyhow::Result;

pub(super) fn render_entry(
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
        let cursor = |items: &[EntryRecord]| {
            items.last().map(|entry| EntryCursor {
                seq: entry.seq,
                entry: entry.id,
            })
        };
        if reply_count < view.replies.len() {
            visible.replies_next_after = cursor(&visible.replies);
        }
        if backref_count < view.backrefs.len() {
            visible.backrefs_next_after = cursor(&visible.backrefs);
        }
        if let Some(proposal) = &mut visible.proposal {
            proposal.body = PlanText::new(body_lines[..body_count].concat())?;
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
        acknowledge_seq: None,
    })
}

pub(super) fn render_plan(
    reply: &BoardReply,
    view: &PlanView,
    budget: &OutputBudget,
) -> Result<RenderedBoard> {
    let body_lines = view
        .revision
        .body
        .as_str()
        .split_inclusive('\n')
        .collect::<Vec<_>>();
    let render = |lines: usize, tasks: usize, claims: usize, entries: usize, commits: usize| {
        let mut visible = view.clone();
        visible.tasks.truncate(tasks);
        visible.claims.truncate(claims);
        visible.entries.truncate(entries);
        visible.commits.truncate(commits);
        visible.tasks_omitted += view.tasks.len() - tasks;
        visible.claims_omitted += view.claims.len() - claims;
        visible.entries_omitted += view.entries.len() - entries;
        visible.commits_omitted += view.commits.len() - commits;
        if tasks < view.tasks.len() {
            visible.tasks_next_after = visible.tasks.last().map(|task| task.id);
        }
        if claims < view.claims.len() {
            visible.claims_next_after = None;
        }
        if entries < view.entries.len() {
            visible.entries_next_after = visible.entries.last().map(|entry| EntryCursor {
                seq: entry.seq,
                entry: entry.id,
            });
        }
        visible.revision.body = PlanText::new(body_lines[..lines].concat())?;
        let mut candidate = reply.clone();
        candidate.result = BoardResult::Plan(visible);
        let next =
            (lines < body_lines.len()).then(|| format!("board show {} -b 32768", view.revision.id));
        render_complete(
            &candidate,
            BoardOmitted {
                entries: view.entries.len() - entries,
                body_lines: body_lines.len() - lines,
                ..BoardOmitted::default()
            },
            None,
            next,
            budget,
        )
    };
    // Claim and task summaries stay useful before the immutable body is expanded.
    let claims = fit_items(view.claims.len(), budget, |count| render(0, 0, count, 0, 0))?;
    let tasks = fit_items(view.tasks.len(), budget, |count| {
        render(0, count, claims, 0, 0)
    })?;
    let lines = fit_items(body_lines.len(), budget, |count| {
        render(count, tasks, claims, 0, 0)
    })?;
    let entries = fit_items(view.entries.len(), budget, |count| {
        render(lines, tasks, claims, count, 0)
    })?;
    let commits = fit_items(view.commits.len(), budget, |count| {
        render(lines, tasks, claims, entries, count)
    })?;
    Ok(RenderedBoard {
        text: require_fits(render(lines, tasks, claims, entries, commits)?, budget)?,
        acknowledge_seq: None,
    })
}
