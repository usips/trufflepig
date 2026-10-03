//! Deterministic review evidence assembly and ordered, explicit budget trimming.

mod history_drills;
#[cfg(test)]
mod tests;
use history_drills::HistoryDrillGate;

use crate::board::board_actor::{HarnessLabel, claim_vendor};
use crate::board::board_ids::PlanRevision;
use crate::board::board_protocol::{
    ClaimRecord, EntryRecord, FeedbackRecord, LinkedCommit, PlanRecord, ProposalRecord,
    RepoScanTarget, ReviewEvidence, TaskRecord,
};
use crate::board::commit_trailers::attributed_to;
use crate::history::source_diff::diff_sources;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewPacket {
    pub plan: PlanRecord,
    pub base: PlanRevision,
    pub head: PlanRevision,
    pub agent: Option<HarnessLabel>,
    pub ssot_diff: SsotDiff,
    pub entries: Vec<EntryRecord>,
    pub tasks: Vec<TaskRecord>,
    pub claims: Vec<ClaimRecord>,
    pub linked: Vec<ReviewCommit>,
    pub unlinked: Vec<ReviewCommit>,
    pub crossed: Vec<CrossedCommit>,
    pub open_proposals: Vec<ProposalRecord>,
    pub open_questions: Vec<EntryRecord>,
    pub open_feedback: Vec<FeedbackRecord>,
    pub scan_errors: Vec<String>,
    pub omitted: ReviewOmitted,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReviewOmitted {
    pub entries: usize,
    pub diff_context_lines: usize,
    pub diff_body_lines: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewCommit {
    #[serde(flatten)]
    pub commit: LinkedCommit,
    pub drill: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CrossedCommit {
    pub repo_key: crate::board::board_ids::RepoKey,
    pub oid: crate::identity::GitOid,
    pub task: crate::board::board_ids::TaskId,
    pub claimant: crate::board::board_actor::BoardActor,
    pub claim_entry: crate::board::board_ids::EntryId,
    pub scope: crate::board::board_vocabulary::EntryText,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SsotDiff {
    pub before: PlanRevision,
    pub after: PlanRevision,
    pub hunks: Vec<SsotHunk>,
    pub next: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SsotHunk {
    pub before_start: usize,
    pub after_start: usize,
    pub removed: Vec<String>,
    pub added: Vec<String>,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
}

/// Assemble evidence only; git scans and board reads belong to the caller.
pub fn assemble_review(
    evidence: &ReviewEvidence,
    agent: Option<&HarnessLabel>,
    repositories: &[RepoScanTarget],
    unlinked: Vec<LinkedCommit>,
    mut scan_errors: Vec<String>,
) -> ReviewPacket {
    let mut entries = evidence
        .entries
        .iter()
        .filter(|entry| agent.is_none_or(|agent| &entry.actor.harness == agent))
        .cloned()
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.seq);
    let mut tasks = evidence.tasks.clone();
    tasks.sort_by_key(|task| task.id.ordinal);
    let mut claims = evidence
        .claims
        .iter()
        .filter(|claim| agent.is_none_or(|agent| &claim.actor.harness == agent))
        .cloned()
        .collect::<Vec<_>>();
    claims.sort_by_key(|claim| (claim.claimed_at, claim.task.ordinal, claim.entry));
    let mut commits = evidence.commits.clone();
    commits.sort_by(|a, b| {
        (a.committed_at, &a.repo_key, &a.oid).cmp(&(b.committed_at, &b.repo_key, &b.oid))
    });
    let mut crossed = Vec::new();
    for commit in &commits {
        for link in &commit.plans {
            if link.plan_id != evidence.plan.id {
                continue;
            }
            let Some(ordinal) = link.task_ordinal else {
                continue;
            };
            for claim in &evidence.claims {
                if claim.task.ordinal != ordinal
                    || claim.task.plan != link.plan_id
                    || commit.committed_at < claim.claimed_at
                    || claim.ended_at.is_some_and(|end| commit.committed_at >= end)
                    || attributed_to(
                        commit,
                        &claim_vendor(&claim.actor.harness, claim.model.as_deref()),
                    )
                {
                    continue;
                }
                crossed.push(CrossedCommit {
                    repo_key: commit.repo_key.clone(),
                    oid: commit.oid,
                    task: claim.task,
                    claimant: claim.actor.clone(),
                    claim_entry: claim.entry,
                    scope: claim.scope.clone(),
                });
            }
        }
    }
    let mut drill_gate = HistoryDrillGate::default();
    let linked = commits
        .into_iter()
        .map(|commit| drill_commit(commit, repositories, &mut drill_gate))
        .collect();
    let mut unlinked = unlinked
        .into_iter()
        .filter(|commit| {
            commit.committed_at >= evidence.base.created_at
                && commit.committed_at <= evidence.window_end
        })
        .collect::<Vec<_>>();
    unlinked.sort_by(|a, b| (&a.repo_key, &a.oid).cmp(&(&b.repo_key, &b.oid)));
    unlinked.dedup_by(|a, b| a.repo_key == b.repo_key && a.oid == b.oid);
    unlinked.sort_by(|a, b| {
        (a.committed_at, &a.repo_key, &a.oid).cmp(&(b.committed_at, &b.repo_key, &b.oid))
    });
    for repository in repositories {
        if let Some(error) = &repository.scan_error {
            scan_errors.push(error.clone());
        }
    }
    scan_errors.sort();
    scan_errors.dedup();
    ReviewPacket {
        plan: evidence.plan.clone(),
        base: evidence.base.id,
        head: evidence.head.id,
        agent: agent.cloned(),
        ssot_diff: build_ssot_diff(&evidence.base, &evidence.head),
        entries,
        tasks,
        claims,
        linked,
        unlinked: unlinked
            .into_iter()
            .map(|commit| drill_commit(commit, repositories, &mut drill_gate))
            .collect(),
        crossed,
        open_proposals: evidence.open_proposals.clone(),
        open_questions: evidence.open_questions.clone(),
        open_feedback: evidence.open_feedback.clone(),
        scan_errors,
        omitted: ReviewOmitted::default(),
    }
}

fn drill_commit(
    commit: LinkedCommit,
    repositories: &[RepoScanTarget],
    gate: &mut HistoryDrillGate,
) -> ReviewCommit {
    let candidates = repositories
        .iter()
        .filter(|target| target.registration.repo_key == commit.repo_key)
        .map(|target| &target.registration.common_dir)
        .collect::<std::collections::BTreeSet<_>>();
    let drill = candidates.into_iter().find_map(|common| {
        let root = if common.file_name().is_some_and(|name| name == ".git") {
            common.parent().unwrap_or(common.as_path())
        } else {
            common.as_path()
        };
        gate.available(root, common, commit.oid).then(|| {
            format!(
                "trufflepig-agent --root {} diff {}",
                shell_path(root),
                commit.oid
            )
        })
    });
    ReviewCommit { commit, drill }
}

fn shell_path(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
}

pub fn build_ssot_diff(
    before: &crate::board::board_protocol::RevisionRecord,
    after: &crate::board::board_protocol::RevisionRecord,
) -> SsotDiff {
    let before_text = before.body.as_str();
    let after_text = after.body.as_str();
    let changes = diff_sources(before_text.as_bytes(), after_text.as_bytes());
    let before_lines = before_text.split_inclusive('\n').collect::<Vec<_>>();
    let after_lines = after_text.split_inclusive('\n').collect::<Vec<_>>();
    let hunks = changes
        .changes
        .iter()
        .enumerate()
        .map(|(index, change)| {
            let start = change.before_lines.start;
            let end = change.before_lines.end;
            let context_start = start.saturating_sub(3).max(
                index
                    .checked_sub(1)
                    .map_or(0, |previous| changes.changes[previous].before_lines.end),
            );
            let context_end = (end + 3).min(
                changes
                    .changes
                    .get(index + 1)
                    .map_or(before_lines.len(), |next| next.before_lines.start),
            );
            SsotHunk {
                before_start: start + 1,
                after_start: change.after_lines.start + 1,
                removed: before_lines[change.before_lines.clone()]
                    .iter()
                    .map(|line| (*line).to_owned())
                    .collect(),
                added: after_lines[change.after_lines.clone()]
                    .iter()
                    .map(|line| (*line).to_owned())
                    .collect(),
                context_before: before_lines[context_start..start]
                    .iter()
                    .map(|line| (*line).to_owned())
                    .collect(),
                context_after: before_lines[end..context_end]
                    .iter()
                    .map(|line| (*line).to_owned())
                    .collect(),
            }
        })
        .collect();
    SsotDiff {
        before: before.id,
        after: after.id,
        hunks,
        next: None,
    }
}

impl ReviewPacket {
    pub(crate) fn trim_diff_context(&mut self) {
        for hunk in &mut self.ssot_diff.hunks {
            self.omitted.diff_context_lines += hunk.context_before.len() + hunk.context_after.len();
            hunk.context_before.clear();
            hunk.context_after.clear();
        }
    }

    pub(crate) fn trim_diff_body(&mut self) {
        self.omitted.diff_body_lines += self
            .ssot_diff
            .hunks
            .iter()
            .map(|hunk| hunk.removed.len() + hunk.added.len())
            .sum::<usize>();
        self.ssot_diff.hunks.clear();
        self.ssot_diff.next = Some(format!(
            "board show {}@{}..{}",
            self.base.plan, self.base.revision, self.head.revision
        ));
    }
}
