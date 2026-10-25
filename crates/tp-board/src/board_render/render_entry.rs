//! Plan and entry views preserve primary bodies while fitting related evidence.
use super::render_task::{
    commit_detail_counts, plan_claim_detail_counts, trim_commit_details, trim_plan_claim_details,
};
use super::{BoardOmitted, RenderedBoard, fit_items, render_complete, require_fits};
use crate::board_protocol::{
    BoardReply, BoardResult, EntryCursor, EntryRecord, EntryView, PlanView,
};
use crate::board_vocabulary::PlanText;
use anyhow::Result;
use tp_core::output::OutputBudget;

#[derive(Clone, Copy)]
struct PlanTrim {
    lines: usize,
    tasks: usize,
    claims: usize,
    grants: usize,
    entries: usize,
    commits: usize,
    worker_files: usize,
    progress_gates: usize,
    workers: usize,
    commit_worker_refs: usize,
    commit_claim_links: usize,
}

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
    let (max_files, max_gates, max_workers) = plan_claim_detail_counts(&view.claims);
    let (max_commit_worker_refs, max_commit_claim_links) = commit_detail_counts(&view.commits);
    let full = PlanTrim {
        lines: body_lines.len(),
        tasks: view.tasks.len(),
        claims: view.claims.len(),
        grants: view.grants.len(),
        entries: view.entries.len(),
        commits: view.commits.len(),
        worker_files: max_files,
        progress_gates: max_gates,
        workers: max_workers,
        commit_worker_refs: max_commit_worker_refs,
        commit_claim_links: max_commit_claim_links,
    };
    let render = |trim: PlanTrim| {
        let mut visible = view.clone();
        visible.tasks.truncate(trim.tasks);
        visible.claims.truncate(trim.claims);
        visible.grants.truncate(trim.grants);
        visible.entries.truncate(trim.entries);
        visible.commits.truncate(trim.commits);
        let nested = trim_plan_claim_details(
            &mut visible.claims,
            trim.worker_files,
            trim.progress_gates,
            trim.workers,
        );
        let (commit_worker_refs, commit_claim_links) = trim_commit_details(
            &mut visible.commits,
            trim.commit_worker_refs,
            trim.commit_claim_links,
        );
        visible.tasks_omitted += view.tasks.len() - trim.tasks;
        visible.claims_omitted += view.claims.len() - trim.claims;
        visible.entries_omitted += view.entries.len() - trim.entries;
        visible.commits_omitted += view.commits.len() - trim.commits;
        if trim.tasks < view.tasks.len() {
            visible.tasks_next_after = visible.tasks.last().map(|task| task.id);
        }
        if trim.claims < view.claims.len() {
            visible.claims_next_after = None;
        }
        if trim.entries < view.entries.len() {
            visible.entries_next_before = visible.entries.last().map(|entry| EntryCursor {
                seq: entry.seq,
                entry: entry.id,
            });
        }
        visible.revision.body = PlanText::new(body_lines[..trim.lines].concat())?;
        let mut candidate = reply.clone();
        candidate.result = BoardResult::Plan(visible);
        let next = (trim.lines < body_lines.len())
            .then(|| format!("board show {} -b 32768", view.revision.id));
        render_complete(
            &candidate,
            BoardOmitted {
                claims: view.claims.len() - trim.claims,
                grants: view.grants.len() - trim.grants,
                entries: view.entries.len() - trim.entries,
                commits: view.commits.len() - trim.commits,
                commit_worker_refs,
                commit_claim_links,
                body_lines: body_lines.len() - trim.lines,
                workers: nested.workers,
                worker_files: nested.worker_files,
                progress_gates: nested.progress_gates,
                ..BoardOmitted::default()
            },
            None,
            next,
            budget,
        )
    };
    // Nested evidence is trimmed while every claim and commit row is present.
    let mut trim = full;
    trim.lines = 0;
    trim.tasks = 0;
    trim.claims = view.claims.len();
    trim.grants = 0;
    trim.entries = 0;
    trim.commits = view.commits.len();
    trim.commit_worker_refs = fit_items(max_commit_worker_refs, budget, |count| {
        render(PlanTrim {
            commit_worker_refs: count,
            ..trim
        })
    })?;
    trim.commit_claim_links = fit_items(max_commit_claim_links, budget, |count| {
        render(PlanTrim {
            commit_claim_links: count,
            ..trim
        })
    })?;
    trim.worker_files = fit_items(max_files, budget, |count| {
        render(PlanTrim {
            worker_files: count,
            ..trim
        })
    })?;
    trim.progress_gates = fit_items(max_gates, budget, |count| {
        render(PlanTrim {
            progress_gates: count,
            ..trim
        })
    })?;
    trim.workers = fit_items(max_workers, budget, |count| {
        render(PlanTrim {
            workers: count,
            ..trim
        })
    })?;
    // Body, tasks and claims outrank commit rows, which take the leftover budget.
    trim.commits = 0;
    // Claim and task summaries stay useful before the immutable body is expanded.
    trim.claims = fit_items(view.claims.len(), budget, |count| {
        render(PlanTrim {
            claims: count,
            ..trim
        })
    })?;
    trim.grants = fit_items(view.grants.len(), budget, |count| {
        render(PlanTrim {
            grants: count,
            ..trim
        })
    })?;
    trim.tasks = fit_items(view.tasks.len(), budget, |count| {
        render(PlanTrim {
            tasks: count,
            ..trim
        })
    })?;
    trim.lines = fit_items(body_lines.len(), budget, |count| {
        render(PlanTrim {
            lines: count,
            ..trim
        })
    })?;
    trim.entries = fit_items(view.entries.len(), budget, |count| {
        render(PlanTrim {
            entries: count,
            ..trim
        })
    })?;
    trim.commits = fit_items(view.commits.len(), budget, |count| {
        render(PlanTrim {
            commits: count,
            ..trim
        })
    })?;
    Ok(RenderedBoard {
        text: require_fits(render(trim)?, budget)?,
        acknowledge_seq: None,
    })
}
