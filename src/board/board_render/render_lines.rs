//! Line-format board results use shared escaped cells and record renderers.

mod collection_lines;
mod diff_lines;
mod line_cells;
mod record_lines;

pub(super) use diff_lines::diff_lines;
pub(super) use line_cells::{cell, footer, recipient};
pub(super) use record_lines::{claim_line, entry_line};

use crate::board::board_protocol::{BoardResult, FeedbackVia, InboxWait};
use line_cells::author;
use record_lines::{plan_lines, revision_lines};
use std::fmt::Write;

pub(super) fn lines_result(result: &BoardResult) -> String {
    let mut text = String::new();
    match result {
        BoardResult::Inbox(inbox) => {
            let first = inbox.events.first().map_or(inbox.cursor, |event| event.seq);
            let last = inbox.events.last().map_or(inbox.cursor, |event| event.seq);
            let plans = inbox
                .events
                .iter()
                .filter_map(|event| event.plan)
                .collect::<std::collections::BTreeSet<_>>()
                .len();
            writeln!(
                text,
                "inbox {} {first}..{last} ({} events, {plans} plans) wait={}",
                cell(&inbox.actor.identity()),
                inbox.events.len(),
                match inbox.wait {
                    InboxWait::None => "none",
                    InboxWait::Ready => "ready",
                    InboxWait::Timeout => "timeout",
                    InboxWait::Busy => "busy",
                }
            )
            .unwrap();
            writeln!(text, "scanned through: {}", inbox.scanned_through).unwrap();
            for event in &inbox.events {
                writeln!(
                    text,
                    "{}\t{}\t{}\t{}\t{}\t{}{}{}",
                    event.seq,
                    event
                        .plan
                        .map_or_else(|| "-".to_owned(), |plan| plan.to_string()),
                    event.subject,
                    event.kind,
                    author(
                        &event.actor,
                        event.model.as_deref(),
                        event.effort.as_deref()
                    ),
                    cell(event.summary.as_str()),
                    recipient(&event.to),
                    match event.via {
                        Some(FeedbackVia::Outbox) => "\tvia=outbox spooled unverified",
                        None => "",
                    }
                )
                .unwrap();
            }
            if !inbox.open.is_empty() {
                text.push_str("--- still open ---\n");
            }
            for entry in &inbox.open {
                entry_line(&mut text, entry);
            }
        }
        BoardResult::Entry(view) => {
            entry_line(&mut text, &view.entry);
            writeln!(
                text,
                "permissions: decide={} supersede={}",
                view.can_decide, view.can_supersede
            )
            .unwrap();
            if let Some(proposal) = &view.proposal {
                writeln!(
                    text,
                    "proposal {} base={}@{} state={}",
                    proposal.entry, proposal.plan, proposal.base_revision, proposal.state
                )
                .unwrap();
                for line in proposal.body.as_str().split_inclusive('\n') {
                    writeln!(text, "| {}", cell(line.trim_end_matches('\n'))).unwrap();
                }
            }
            for entry in &view.replies {
                entry_line(&mut text, entry);
            }
            for entry in &view.backrefs {
                entry_line(&mut text, entry);
            }
            writeln!(
                text,
                "omitted: replies={} backrefs={}",
                view.replies_omitted, view.backrefs_omitted
            )
            .unwrap();
        }
        BoardResult::Plan(view) => plan_lines(&mut text, view),
        BoardResult::Revision(revision) => revision_lines(&mut text, revision),
        BoardResult::Search(result) => {
            writeln!(text, "search truncated={}", result.truncated).unwrap();
            for hit in &result.hits {
                writeln!(
                    text,
                    "{}\t{:?}\t{}",
                    hit.target,
                    hit.source,
                    cell(&hit.snippet)
                )
                .unwrap();
            }
        }
        BoardResult::Session(session) => writeln!(
            text,
            "hello {} model={} effort={} cursor={}",
            cell(&session.actor.identity()),
            cell(session.model.as_deref().unwrap_or("unclaimed")),
            cell(session.effort.as_deref().unwrap_or("unclaimed")),
            session.cursor
        )
        .unwrap(),
        BoardResult::Cursor(seq) => writeln!(text, "cursor {seq}").unwrap(),
        BoardResult::Change(change) => writeln!(
            text,
            "{} seq={} plan={} revision={} task={} deduplicated={}",
            change.entry,
            change.seq,
            change.plan.map_or_else(|| "-".into(), |v| v.to_string()),
            change
                .revision
                .map_or_else(|| "-".into(), |v| v.to_string()),
            change.task.map_or_else(|| "-".into(), |v| v.to_string()),
            change.deduplicated
        )
        .unwrap(),
        BoardResult::Feedback(feedback) => {
            text.push_str("feedback\n");
            for report in &feedback.feedback {
                writeln!(
                    text,
                    "{}\t{}\t{}",
                    report.entry.id, report.kind, report.state
                )
                .unwrap();
                entry_line(&mut text, &report.entry);
            }
        }
        BoardResult::Overview(_)
        | BoardResult::Attention(_)
        | BoardResult::Feed(_)
        | BoardResult::History(_)
        | BoardResult::Entries(_)
        | BoardResult::Tasks(_)
        | BoardResult::Claims(_) => collection_lines::lines(&mut text, result),
        BoardResult::Repositories(repositories) => {
            for repository in repositories {
                writeln!(
                    text,
                    "repo {}\t{}\t{}",
                    repository.registration.repo_key,
                    cell(&repository.registration.common_dir.to_string_lossy()),
                    cell(repository.scan_error.as_deref().unwrap_or("ready"))
                )
                .unwrap();
            }
        }
        BoardResult::Registered(repository) => writeln!(
            text,
            "registered {}\t{}",
            repository.repo_key,
            cell(&repository.common_dir.to_string_lossy())
        )
        .unwrap(),
        BoardResult::CommitsLinked(result) => {
            writeln!(text, "ingest inserted={}", result.inserted).unwrap();
            for plan in &result.unknown_plans {
                writeln!(text, "unknown plan: {plan}").unwrap();
            }
            for task in &result.unknown_tasks {
                writeln!(text, "unknown task: {task}; commit linked to its plan").unwrap();
            }
        }
        BoardResult::Queued { import_key } => writeln!(
            text,
            "queued: pending import ({})",
            cell(&import_key.to_string())
        )
        .unwrap(),
        BoardResult::ScanRecorded => text.push_str("scan recorded\n"),
        BoardResult::RepoPathForgotten => text.push_str("repository path removed\n"),
        BoardResult::Diff(_) | BoardResult::Review(_) => {
            unreachable!("diffs and reviews have dedicated renderers")
        }
    }
    text
}
