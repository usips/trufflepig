//! Task detail rendering keeps the task and current claim ahead of nested history.

use super::{BoardOmitted, RenderedBoard, fit_items, render_complete, require_fits};
use crate::board_protocol::{BoardReply, BoardResult, ClaimRecord, LinkedCommit, TaskView};
use anyhow::Result;
use tp_core::output::{OutputBudget, OutputFormat};

#[derive(Clone, Copy)]
struct TaskTrim {
    files: usize,
    gates: usize,
    workers: usize,
    commit_worker_refs: usize,
    commit_claim_links: usize,
    claims: usize,
    entries: usize,
    commits: usize,
    deduplicate_shared_progress: bool,
}

pub(super) fn render_task(
    reply: &BoardReply,
    view: &TaskView,
    budget: &OutputBudget,
) -> Result<RenderedBoard> {
    let full = TaskTrim {
        files: file_count(view),
        gates: gate_count(view),
        workers: worker_count(view),
        commit_worker_refs: commit_worker_ref_count(&view.commits),
        commit_claim_links: commit_claim_link_count(&view.commits),
        claims: view.claim_history.len(),
        entries: view.entries.len(),
        commits: view.commits.len(),
        deduplicate_shared_progress: false,
    };
    let render = |trim: TaskTrim| {
        let (visible, omitted) = trimmed_view(view, trim);
        let mut candidate = reply.clone();
        candidate.result = BoardResult::TaskView(visible);
        render_complete(&candidate, omitted, None, None, budget)
    };

    // Fit the primary claim's worker before adding older claims or evidence.
    // The newest entry remains beside it as an anchor while worker details fit.
    let mut trim = TaskTrim {
        files: 0,
        gates: 0,
        workers: 0,
        commit_worker_refs: 0,
        commit_claim_links: 0,
        claims: 0,
        entries: full.entries.min(1),
        commits: 0,
        deduplicate_shared_progress: false,
    };
    trim.workers = fit_items(full.workers, budget, |workers| {
        render(TaskTrim { workers, ..trim })
    })?;
    if trim.workers == 0 && full.workers > 0 && budget.format == OutputFormat::Json {
        // A claim and its worker can expose the same progress snapshot. The
        // claim fact names the worker, so it can carry that evidence once.
        trim.deduplicate_shared_progress = true;
        trim.workers = fit_items(full.workers, budget, |workers| {
            render(TaskTrim { workers, ..trim })
        })?;
    }
    trim.files = fit_items(full.files, budget, |files| {
        render(TaskTrim { files, ..trim })
    })?;
    trim.gates = fit_items(full.gates, budget, |gates| {
        render(TaskTrim { gates, ..trim })
    })?;
    trim.entries = fit_items(full.entries, budget, |entries| {
        render(TaskTrim { entries, ..trim })
    })?;
    trim.claims = fit_items(full.claims, budget, |claims| {
        render(TaskTrim { claims, ..trim })
    })?;
    trim.commits = fit_items(full.commits, budget, |commits| {
        render(TaskTrim { commits, ..trim })
    })?;
    trim.commit_worker_refs = fit_items(full.commit_worker_refs, budget, |refs| {
        render(TaskTrim {
            commit_worker_refs: refs,
            ..trim
        })
    })?;
    trim.commit_claim_links = fit_items(full.commit_claim_links, budget, |links| {
        render(TaskTrim {
            commit_claim_links: links,
            ..trim
        })
    })?;

    let (visible, omitted) = trimmed_view(view, trim);
    let mut candidate = reply.clone();
    candidate.result = BoardResult::TaskView(visible);
    Ok(RenderedBoard {
        text: require_fits(
            render_complete(&candidate, omitted, None, None, budget)?,
            budget,
        )?,
        acknowledge_seq: None,
    })
}

fn trimmed_view(view: &TaskView, trim: TaskTrim) -> (TaskView, BoardOmitted) {
    let mut visible = view.clone();
    visible
        .entries
        .sort_by(|left, right| right.seq.cmp(&left.seq));
    visible
        .commits
        .sort_by(|left, right| right.committed_at.cmp(&left.committed_at));

    if trim.deduplicate_shared_progress {
        for claim in visible
            .current_claim
            .iter_mut()
            .chain(&mut visible.claim_history)
        {
            deduplicate_shared_worker_progress(claim);
        }
    }

    let claims_omitted = visible.claim_history.len().saturating_sub(trim.claims);
    visible.claim_history.truncate(trim.claims);
    visible.claims_omitted += claims_omitted;

    let commits_omitted = visible.commits.len().saturating_sub(trim.commits);
    visible.commits.truncate(trim.commits);
    visible.commits_omitted += commits_omitted;
    let (commit_worker_refs_omitted, commit_claim_links_omitted) = trim_commit_details(
        &mut visible.commits,
        trim.commit_worker_refs,
        trim.commit_claim_links,
    );

    let mut worker_budget = trim.workers;
    let mut workers_omitted = 0;
    if let Some(claim) = &mut visible.current_claim {
        trim_claim_workers(claim, &mut worker_budget, &mut workers_omitted);
    }
    for claim in &mut visible.claim_history {
        trim_claim_workers(claim, &mut worker_budget, &mut workers_omitted);
    }

    let mut file_budget = trim.files;
    let mut worker_files_omitted = 0;
    let mut gate_budget = trim.gates;
    let mut progress_gates_omitted = 0;
    if let Some(claim) = &mut visible.current_claim {
        trim_claim_details(
            claim,
            &mut file_budget,
            &mut worker_files_omitted,
            &mut gate_budget,
            &mut progress_gates_omitted,
        );
    }
    for claim in &mut visible.claim_history {
        trim_claim_details(
            claim,
            &mut file_budget,
            &mut worker_files_omitted,
            &mut gate_budget,
            &mut progress_gates_omitted,
        );
    }

    let entries_omitted = visible.entries.len().saturating_sub(trim.entries);
    visible.entries.truncate(trim.entries);
    visible.entries_omitted += entries_omitted;
    (
        visible,
        BoardOmitted {
            claims: claims_omitted,
            entries: entries_omitted,
            commits: commits_omitted,
            commit_worker_refs: commit_worker_refs_omitted,
            commit_claim_links: commit_claim_links_omitted,
            workers: workers_omitted,
            worker_files: worker_files_omitted,
            progress_gates: progress_gates_omitted,
            ..BoardOmitted::default()
        },
    )
}

fn trim_claim_workers(claim: &mut ClaimRecord, remaining: &mut usize, omitted: &mut usize) {
    let keep = claim.workers.len().min(*remaining);
    let removed = claim.workers.len() - keep;
    claim.workers.truncate(keep);
    claim.workers_omitted += removed;
    *remaining -= keep;
    *omitted += removed;
}

fn deduplicate_shared_worker_progress(claim: &mut ClaimRecord) {
    let Some(progress) = &claim.progress else {
        return;
    };
    for worker in &mut claim.workers {
        if progress.worker.as_ref() == Some(&worker.address)
            && worker.progress.as_ref() == Some(progress)
        {
            worker.progress = None;
        }
    }
}

fn trim_claim_details(
    claim: &mut ClaimRecord,
    file_budget: &mut usize,
    files_omitted: &mut usize,
    gate_budget: &mut usize,
    gates_omitted: &mut usize,
) {
    if let Some(progress) = &mut claim.progress {
        let keep = progress.gates.len().min(*gate_budget);
        *gates_omitted += progress.gates.len() - keep;
        progress.gates.truncate(keep);
        *gate_budget -= keep;
    }
    for worker in &mut claim.workers {
        let keep = worker.files.len().min(*file_budget);
        *files_omitted += worker.files.len() - keep;
        worker.files.truncate(keep);
        *file_budget -= keep;
        if let Some(progress) = &mut worker.progress {
            let keep = progress.gates.len().min(*gate_budget);
            *gates_omitted += progress.gates.len() - keep;
            progress.gates.truncate(keep);
            *gate_budget -= keep;
        }
    }
}

pub(super) fn plan_claim_detail_counts(claims: &[ClaimRecord]) -> (usize, usize, usize) {
    let mut files = 0;
    let mut gates = 0;
    let mut workers = 0;
    for claim in claims {
        workers += claim.workers.len();
        files += claim
            .workers
            .iter()
            .map(|worker| worker.files.len())
            .sum::<usize>();
        gates += claim
            .progress
            .as_ref()
            .map_or(0, |progress| progress.gates.len());
        gates += claim
            .workers
            .iter()
            .map(|worker| {
                worker
                    .progress
                    .as_ref()
                    .map_or(0, |progress| progress.gates.len())
            })
            .sum::<usize>();
    }
    (files, gates, workers)
}

pub(super) fn commit_detail_counts(commits: &[LinkedCommit]) -> (usize, usize) {
    (
        commit_worker_ref_count(commits),
        commit_claim_link_count(commits),
    )
}

pub(super) fn trim_commit_details(
    commits: &mut [LinkedCommit],
    worker_refs: usize,
    claim_links: usize,
) -> (usize, usize) {
    let mut refs_remaining = worker_refs;
    let mut refs_omitted = 0;
    let mut links_remaining = claim_links;
    let mut links_omitted = 0;
    for commit in commits {
        let keep = commit.worker_refs.len().min(refs_remaining);
        refs_omitted += commit.worker_refs.len() - keep;
        commit.worker_refs.truncate(keep);
        refs_remaining -= keep;

        let keep = commit.claim_links.len().min(links_remaining);
        links_omitted += commit.claim_links.len() - keep;
        commit.claim_links.truncate(keep);
        links_remaining -= keep;
    }
    (refs_omitted, links_omitted)
}

pub(super) fn trim_plan_claim_details(
    claims: &mut [ClaimRecord],
    files: usize,
    gates: usize,
    workers: usize,
) -> BoardOmitted {
    let mut remaining_workers = workers;
    let mut workers_omitted = 0;
    for claim in claims.iter_mut() {
        trim_claim_workers(claim, &mut remaining_workers, &mut workers_omitted);
    }
    let mut remaining_files = files;
    let mut worker_files_omitted = 0;
    let mut remaining_gates = gates;
    let mut progress_gates_omitted = 0;
    for claim in claims.iter_mut() {
        trim_claim_details(
            claim,
            &mut remaining_files,
            &mut worker_files_omitted,
            &mut remaining_gates,
            &mut progress_gates_omitted,
        );
    }
    BoardOmitted {
        workers: workers_omitted,
        worker_files: worker_files_omitted,
        progress_gates: progress_gates_omitted,
        ..BoardOmitted::default()
    }
}

fn file_count(view: &TaskView) -> usize {
    claims(view)
        .flat_map(|claim| claim.workers.iter())
        .map(|worker| worker.files.len())
        .sum()
}

fn gate_count(view: &TaskView) -> usize {
    claims(view)
        .map(|claim| {
            claim
                .progress
                .as_ref()
                .map_or(0, |progress| progress.gates.len())
                + claim
                    .workers
                    .iter()
                    .map(|worker| {
                        worker
                            .progress
                            .as_ref()
                            .map_or(0, |progress| progress.gates.len())
                    })
                    .sum::<usize>()
        })
        .sum()
}

fn worker_count(view: &TaskView) -> usize {
    claims(view).map(|claim| claim.workers.len()).sum()
}

fn commit_worker_ref_count(commits: &[LinkedCommit]) -> usize {
    commits.iter().map(|commit| commit.worker_refs.len()).sum()
}

fn commit_claim_link_count(commits: &[LinkedCommit]) -> usize {
    commits.iter().map(|commit| commit.claim_links.len()).sum()
}

fn claims(view: &TaskView) -> impl Iterator<Item = &ClaimRecord> {
    view.current_claim.iter().chain(view.claim_history.iter())
}
