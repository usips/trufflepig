//! Typed immutable plan history, entry references, and review evidence.

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use rusqlite::{Connection, Params, Row, Transaction, params};

use super::{
    BoardError, WriteContext, actor_from_row, invalid, require_plan, row_number, sql_error,
    sql_number, sqlite_u64, task_claims,
};
use crate::board::board_actor::{BoardRecipient, HarnessLabel};
use crate::board::board_ids::{BoardRef, EntryId, EventSeq, PlanId, PlanRevision, RepoKey};
use crate::board::board_protocol::*;
use crate::board::board_vocabulary::{EntryKind, EntryText, PlanText, PlanTitle, ProposalState};

mod entry_reference_reads;
mod plan_history_reads;
mod repository_reads;
mod review_evidence_reads;

pub(super) use entry_reference_reads::{entries, entry, entry_view, open_entries};
use plan_history_reads::{plan, plan_view, plans, revision};
pub(super) use repository_reads::repositories;
pub(super) use review_evidence_reads::review;

const RECENT_ENTRIES: i64 = 20;
const PLAN_SELECT: &str = "SELECT id,title,owner_user,steward,head_revision,created_at FROM plans";
const OPEN_QUESTION: &str = "e.kind='question' AND NOT EXISTS(SELECT 1 FROM entries answer JOIN entry_refs reference ON reference.entry_id=answer.id WHERE answer.kind='answer' AND reference.target='E'||e.id) AND NOT EXISTS(SELECT 1 FROM entries correction WHERE correction.supersedes=e.id)";

pub(super) fn show(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    target: Option<&BoardRef>,
) -> Result<BoardReply, BoardError> {
    let result = match target {
        None => BoardResult::Plans(plans(tx)?),
        Some(BoardRef::Plan(plan)) => BoardResult::Plan(plan_view(tx, ctx, *plan)?),
        Some(BoardRef::Task(task)) => {
            let exists: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM tasks WHERE plan_id=?1 AND ordinal=?2)",
                    params![sql_number(task.plan.get()), sql_number(task.ordinal)],
                    |row| row.get(0),
                )
                .map_err(sql_error)?;
            if !exists {
                return Err(invalid("invalid_reference", format!("unknown task {task}")));
            }
            BoardResult::Plan(plan_view(tx, ctx, task.plan)?)
        }
        Some(BoardRef::Entry(id)) => BoardResult::Entry(entry_view(tx, ctx, *id)?),
        Some(BoardRef::Revision(id)) => BoardResult::Revision(revision(tx, *id)?),
        Some(BoardRef::Span(span)) => {
            let plan = plan(tx, span.plan)?;
            let end = span.end.unwrap_or(plan.head_revision);
            if end < span.start {
                return Err(invalid("invalid_reference", "revision range is reversed"));
            }
            BoardResult::Diff(RevisionDiff {
                before: revision(
                    tx,
                    PlanRevision::new(span.plan, span.start).map_err(BoardError::from)?,
                )?,
                after: revision(
                    tx,
                    PlanRevision::new(span.plan, end).map_err(BoardError::from)?,
                )?,
            })
        }
        Some(_) => {
            return Err(invalid(
                "invalid_reference",
                "show requires a plan, task, entry, revision, or revision span",
            ));
        }
    };
    Ok(BoardReply::new("local", result))
}

fn decode_json<T: serde::de::DeserializeOwned>(body: String) -> Result<T, BoardError> {
    serde_json::from_str(&body).map_err(|error| invalid("board_unavailable", error.to_string()))
}
