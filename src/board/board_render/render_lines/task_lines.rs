//! Task details expose claim workers, field-level progress, and linked commits.

use super::line_cells::{author, source_suffix, vendor_suffix};
use super::{cell, claim_line, entry_line};
use crate::board::board_protocol::{
    ClaimRecord, CommitClaimBasis, GateStatus, LinkedCommit, PendingReply, ProgressEvidence,
    ProgressGateFact, TaskView, WorkerRecord,
};
use std::fmt::Write;

pub(super) fn task_lines(text: &mut String, view: &TaskView) {
    let task = &view.task;
    writeln!(
        text,
        "task {}\t{}\t{}\tsection={} assignee={} seq={} completed={}",
        task.id,
        task.column,
        cell(task.title.as_str()),
        task.section.as_deref().map_or_else(|| "-".into(), cell),
        task.assignee
            .as_ref()
            .map_or_else(|| "-".into(), |assignee| cell(assignee.as_str())),
        task.seq,
        task.done_at.map_or_else(|| "-".into(), |at| at.to_string())
    )
    .unwrap();
    writeln!(
        text,
        "task evidence questions={} oldest_question={} oldest_question_at={} latest_commit_at={} through={} server_now={} claim_ttl_secs={} omitted_claims={} omitted_entries={} omitted_commits={}",
        view.open_questions,
        view.oldest_open_question.map_or_else(|| "-".into(), |entry| entry.to_string()),
        view.oldest_open_question_at.map_or_else(|| "-".into(), |at| at.to_string()),
        view.latest_commit_at.map_or_else(|| "-".into(), |at| at.to_string()),
        view.through,
        view.server_now,
        view.claim_ttl_secs,
        view.claims_omitted,
        view.entries_omitted,
        view.commits_omitted
    )
    .unwrap();

    if let Some(claim) = &view.current_claim {
        text.push_str("--- current claim ---\n");
        claim_details(text, claim, view.server_now);
    }
    if !view.claim_history.is_empty() {
        text.push_str("--- claim history ---\n");
        for claim in &view.claim_history {
            claim_details(text, claim, view.server_now);
        }
    }
    if !view.entries.is_empty() {
        text.push_str("--- recent task entries ---\n");
        for entry in &view.entries {
            entry_line(text, entry);
        }
    }
    if !view.commits.is_empty() {
        text.push_str("--- linked commits ---\n");
        for commit in &view.commits {
            commit_lines(text, commit);
        }
    }
}

fn claim_details(text: &mut String, claim: &ClaimRecord, now: i64) {
    claim_line(text, claim);
    writeln!(
        text,
        "claim evidence ended_at={} end_reason={} questions={} oldest_question={} oldest_question_at={} commits={} latest_commit={} latest_commit_at={} workers_omitted={}",
        claim.ended_at.map_or_else(|| "-".into(), |at| at.to_string()),
        claim.end_reason.map_or("-", |reason| reason.as_str()),
        claim.open_questions,
        claim.oldest_open_question.map_or_else(|| "-".into(), |entry| entry.to_string()),
        claim.oldest_open_question_at.map_or_else(|| "-".into(), |at| at.to_string()),
        claim.commit_count,
        claim.latest_commit.map_or_else(|| "-".into(), |oid| oid.to_string()),
        claim.latest_commit_at.map_or_else(|| "-".into(), |at| at.to_string()),
        claim.workers_omitted
    )
    .unwrap();
    if let Some(progress) = &claim.progress {
        progress_lines(text, "claim", progress, now);
    }
    for worker in &claim.workers {
        worker_lines(text, worker, now);
    }
}

fn worker_lines(text: &mut String, worker: &WorkerRecord, now: i64) {
    let actor = worker
        .actor
        .as_ref()
        .map_or_else(|| "unbound".into(), |actor| cell(&actor.identity()));
    let files = if worker.files.is_empty() {
        "-".into()
    } else {
        worker
            .files
            .iter()
            .map(|path| cell(path))
            .collect::<Vec<_>>()
            .join(",")
    };
    writeln!(
        text,
        "worker {} actor={} role={} model={} effort={} scope={} files={} state={} age={}s added_age={}s ended_at={} verdict={} note={}",
        worker.address,
        actor,
        worker.role,
        cell(&worker.model),
        worker.effort.as_deref().map_or_else(|| "-".into(), cell),
        cell(worker.scope.as_str()),
        files,
        worker.state.map_or("active", |state| state.as_str()),
        age(now, worker.last_active),
        age(now, worker.added_at),
        worker.ended_at.map_or_else(|| "-".into(), |at| at.to_string()),
        worker
            .verdict
            .map_or_else(|| "-".into(), |verdict| verdict.to_string()),
        worker.note.as_ref().map_or_else(|| "-".into(), |note| cell(note.as_str()))
    )
    .unwrap();
    if let Some(progress) = &worker.progress {
        progress_lines(text, &format!("worker {}", worker.address), progress, now);
    }
}

fn progress_lines(text: &mut String, owner: &str, progress: &ProgressEvidence, now: i64) {
    writeln!(
        text,
        "progress {owner} claim={} worker={}",
        progress.claim,
        progress
            .worker
            .as_ref()
            .map_or_else(|| "-".into(), |worker| worker.to_string())
    )
    .unwrap();
    if let Some(head) = &progress.head {
        writeln!(
            text,
            "  head={} verified={} entry={} age={}s",
            head.value.oid,
            head.value.verified,
            head.entry,
            age(now, head.updated_at)
        )
        .unwrap();
    }
    if let Some(step) = &progress.step {
        writeln!(
            text,
            "  step={}/{} entry={} age={}s",
            step.value.current,
            step.value.total,
            step.entry,
            age(now, step.updated_at)
        )
        .unwrap();
    }
    if let Some(tests) = &progress.tests {
        writeln!(
            text,
            "  tests=passed:{} failed:{} entry={} age={}s",
            tests.value.passed,
            tests.value.failed,
            tests.entry,
            age(now, tests.updated_at)
        )
        .unwrap();
    }
    for gate in &progress.gates {
        gate_line(text, gate, now);
    }
    if let Some(eta) = &progress.eta_at {
        writeln!(
            text,
            "  eta_at={} entry={} age={}s",
            eta.value,
            eta.entry,
            age(now, eta.updated_at)
        )
        .unwrap();
    }
}

fn gate_line(text: &mut String, gate: &ProgressGateFact, now: i64) {
    writeln!(
        text,
        "  gate={} status={} entry={} age={}s",
        cell(&gate.name),
        match gate.status {
            GateStatus::Pass => "pass",
            GateStatus::Fail => "fail",
            GateStatus::Running => "running",
        },
        gate.entry,
        age(now, gate.updated_at)
    )
    .unwrap();
}

fn commit_lines(text: &mut String, commit: &LinkedCommit) {
    writeln!(
        text,
        "commit {} repo={} committed_at={} author={} files={} insertions={} deletions={} subject={}",
        commit.oid,
        commit.repo_key,
        commit.committed_at,
        cell(&commit.author),
        commit.files,
        commit.insertions,
        commit.deletions,
        cell(&commit.subject)
    )
    .unwrap();
    for coauthor in &commit.coauthors {
        writeln!(
            text,
            "  coauthor harness={} model={} email={}",
            cell(coauthor.harness.as_str()),
            cell(&coauthor.model),
            cell(&coauthor.email)
        )
        .unwrap();
    }
    for worker in &commit.worker_refs {
        writeln!(text, "  worker_ref {worker}").unwrap();
    }
    for link in &commit.claim_links {
        writeln!(
            text,
            "  claim_link task={} claim={} basis={} actor={}",
            link.task,
            link.claim,
            match link.basis {
                CommitClaimBasis::ClaimWindow => "claim_window",
            },
            link.actor
                .as_ref()
                .map_or_else(|| "-".into(), |actor| cell(&actor.identity()))
        )
        .unwrap();
    }
    for plan in &commit.plans {
        writeln!(
            text,
            "  plan_link {} task_ordinal={}",
            plan.plan_id,
            plan.task_ordinal
                .map_or_else(|| "-".into(), |value| value.to_string())
        )
        .unwrap();
    }
}

fn age(now: i64, then: i64) -> u64 {
    now.saturating_sub(then).max(0) as u64
}

pub(super) fn pending_lines(text: &mut String, reply: &PendingReply) {
    writeln!(
        text,
        "pending after={} through={} generation={} server_now={} notices={} omitted={}",
        reply
            .after
            .map_or_else(|| "-".into(), |seq| seq.to_string()),
        reply.through,
        cell(&reply.generation),
        reply.server_now,
        reply.notices.len(),
        reply.omitted
    )
    .unwrap();
    for notice in &reply.notices {
        match (&notice.entry, &notice.event) {
            (Some(entry), None) => {
                writeln!(
                    text,
                    "{}\tkind={} reason={}",
                    notice.seq,
                    match notice.kind {
                        crate::board::board_protocol::PendingNoticeKind::Entry => "entry",
                        crate::board::board_protocol::PendingNoticeKind::Event => "event",
                    },
                    match notice.reason {
                        crate::board::board_protocol::PendingNoticeReason::Mention => "mention",
                        crate::board::board_protocol::PendingNoticeReason::Question => "question",
                        crate::board::board_protocol::PendingNoticeReason::Claim => "claim",
                        crate::board::board_protocol::PendingNoticeReason::Worker => "worker",
                    }
                )
                .unwrap();
                entry_line(text, entry);
            }
            (None, Some(event)) => {
                writeln!(
                    text,
                    "{}\tkind=event reason={} payload=",
                    notice.seq,
                    match notice.reason {
                        crate::board::board_protocol::PendingNoticeReason::Mention => "mention",
                        crate::board::board_protocol::PendingNoticeReason::Question => "question",
                        crate::board::board_protocol::PendingNoticeReason::Claim => "claim",
                        crate::board::board_protocol::PendingNoticeReason::Worker => "worker",
                    }
                )
                .unwrap();
                event_line(text, event);
            }
            _ => unreachable!("pending notice validation requires exactly one payload"),
        }
    }
}

fn event_line(text: &mut String, event: &crate::board::board_protocol::EventRecord) {
    writeln!(
        text,
        "event {}\t{}\t{}\t{}{}{}",
        event.seq,
        event.kind,
        author(
            &event.actor,
            event.model.as_deref(),
            event.effort.as_deref()
        ),
        cell(event.summary.as_str()),
        vendor_suffix(event.vendor),
        source_suffix(event.origin.as_ref(), event.authority.as_ref())
    )
    .unwrap();
}
