//! Bounded collection pages retain their sequence boundaries in line output.
use super::{author, cell, claim_line, entry_line};
use crate::board::board_protocol::{BoardResult, FeedbackVia};
use std::fmt::Write;

pub(super) fn lines(text: &mut String, result: &BoardResult) {
    match result {
        BoardResult::Overview(page) => {
            writeln!(
                text,
                "overview through={} omitted={}",
                page.through, page.omitted
            )
            .unwrap();
            for item in &page.plans {
                writeln!(
                    text,
                    "{}\t{}\tquestions={} proposals={} feedback={} tasks_omitted={} claims_omitted={}",
                    item.plan.id,
                    cell(item.plan.title.as_str()),
                    item.open_questions,
                    item.open_proposals,
                    item.open_feedback,
                    item.tasks_omitted,
                    item.claims_omitted
                )
                .unwrap();
            }
        }
        BoardResult::Attention(page) => {
            writeln!(
                text,
                "attention {} through={} entries_omitted={} claims_omitted={}",
                cell(&page.actor.identity()),
                page.through,
                page.entries_omitted,
                page.claims_omitted
            )
            .unwrap();
            for entry in &page.entries {
                entry_line(text, entry);
                if page.rebase_needed.contains(&entry.id) {
                    writeln!(text, "rebase needed: {}", entry.id).unwrap();
                }
            }
            for item in &page.stale_claims {
                claim_line(text, &item.claim);
            }
        }
        BoardResult::Feed(page) => {
            writeln!(text, "feed after={} through={}", page.after, page.through).unwrap();
            for event in &page.events {
                writeln!(
                    text,
                    "{}\t{}\t{}\t{}\t{}{}",
                    event.seq,
                    event.subject,
                    event.kind,
                    author(
                        &event.actor,
                        event.model.as_deref(),
                        event.effort.as_deref()
                    ),
                    cell(event.summary.as_str()),
                    match event.via {
                        Some(FeedbackVia::Outbox) => "\tvia=outbox spooled unverified",
                        None => "",
                    }
                )
                .unwrap();
            }
        }
        BoardResult::History(page) => {
            writeln!(
                text,
                "history {} after={} through={}",
                page.plan, page.after, page.through
            )
            .unwrap();
            for revision in &page.revisions {
                writeln!(
                    text,
                    "{}\t{:?}\t{}\t{}\t{}",
                    revision.id,
                    revision.source,
                    cell(&revision.actor.identity()),
                    revision.seq,
                    cell(revision.summary.as_str())
                )
                .unwrap();
            }
        }
        BoardResult::Entries(page) => {
            writeln!(text, "entries through={}", page.through).unwrap();
            for entry in &page.entries {
                entry_line(text, entry);
            }
        }
        BoardResult::Tasks(page) => {
            writeln!(
                text,
                "tasks {} through={} omitted={}",
                page.plan, page.through, page.omitted
            )
            .unwrap();
            for task in &page.tasks {
                writeln!(
                    text,
                    "{}\t{}\t{}",
                    task.id,
                    task.column,
                    cell(task.title.as_str())
                )
                .unwrap();
            }
        }
        BoardResult::DoneTasks(page) => {
            writeln!(text, "done omitted={}", page.omitted).unwrap();
            for task in &page.tasks {
                writeln!(
                    text,
                    "{}\t{}\tcompleted={}",
                    task.id,
                    cell(task.title.as_str()),
                    task.done_at
                        .map_or_else(|| "unknown".into(), |time| time.to_string())
                )
                .unwrap();
            }
        }
        BoardResult::Claims(page) => {
            writeln!(
                text,
                "claims through={} omitted={}",
                page.through, page.omitted
            )
            .unwrap();
            for item in &page.claims {
                claim_line(text, &item.claim);
            }
        }
        _ => unreachable!("collection lines require a collection"),
    }
}
