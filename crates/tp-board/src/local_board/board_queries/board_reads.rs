//! Typed immutable plan history, entry references, and review evidence.

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use rusqlite::{Connection, Params, Row, Transaction, params};

use super::super::{
    BoardError, WriteContext, actor_from_row, invalid, require_plan, row_number, sql_error,
    sql_number, sqlite_u64,
};
use crate::board_actor::BoardRecipient;
use crate::board_ids::{BoardRef, EntryId, EventSeq, PlanId, PlanRevision, RepoKey};
use crate::board_protocol::*;
use crate::board_vocabulary::{EntryKind, EntryText, PlanText, PlanTitle, ProposalState};
use crate::local_board::board_writes::task_claims;
use tp_core::request_context::HarnessLabel;

mod entry_reference_reads;
mod plan_history_reads;
pub(in crate::local_board) mod recipient_predicates;
mod repository_reads;
mod review_evidence_reads;
pub(in crate::local_board) mod task_lineage;
mod task_view_reads;

pub(in crate::local_board) use entry_reference_reads::{entries, entry, entry_view, open_entries};
pub(in crate::local_board) use plan_history_reads::plan_row;
use plan_history_reads::{plan, plan_view, revision};
pub(in crate::local_board) use repository_reads::repositories;
pub(in crate::local_board) use review_evidence_reads::review;
pub(in crate::local_board) use task_view_reads::task_view;

const RECENT_ENTRIES: i64 = 20;
const PLAN_ENTRY_CAP: u64 = 200;
const PLAN_SELECT: &str =
    "SELECT id,title,owner_user,head_revision,created_at,lifecycle_state FROM plans";
const OPEN_QUESTION: &str = concat!(
    "e.kind='question' AND NOT EXISTS(SELECT 1 FROM entries answer JOIN entry_refs reference ",
    "ON reference.entry_id=answer.id WHERE answer.kind='answer' AND reference.target='E'||e.id) ",
    "AND NOT EXISTS(SELECT 1 FROM entries correction WHERE correction.supersedes=e.id)"
);

pub(in crate::local_board) fn show(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    target: &BoardRef,
) -> Result<BoardReply, BoardError> {
    let result = match target {
        BoardRef::Plan(plan) => BoardResult::Plan(plan_view(tx, ctx, *plan)?),
        BoardRef::Task(task) => BoardResult::TaskView(task_view(tx, ctx, *task)?),
        BoardRef::Entry(id) => BoardResult::Entry(entry_view(tx, ctx, *id)?),
        BoardRef::Revision(id) => BoardResult::Revision(revision(tx, *id)?),
        BoardRef::Span(span) => {
            let plan = plan(tx, span.plan)?;
            let end = span.end.unwrap_or(plan.head_revision);
            if end < span.start {
                return Err(invalid(
                    BoardErrorCode::InvalidReference,
                    "revision range is reversed",
                ));
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
        _ => {
            return Err(invalid(
                BoardErrorCode::InvalidReference,
                "show requires a plan, task, entry, revision, or revision span",
            ));
        }
    };
    Ok(BoardReply::new("local", result))
}

fn decode_json<T: serde::de::DeserializeOwned>(body: String) -> Result<T, BoardError> {
    serde_json::from_str(&body)
        .map_err(|error| invalid(BoardErrorCode::BoardUnavailable, error.to_string()))
}

pub(in crate::local_board) fn linked_commits_bounded(
    conn: &Connection,
    plan: PlanId,
    start: i64,
    end: i64,
    limit: usize,
) -> Result<(Vec<LinkedCommit>, usize), BoardError> {
    super::board_commits::linked_commits_bounded(conn, plan, start, end, limit)
}

pub(in crate::local_board) fn linked_commit_for_entry(
    conn: &Connection,
    entry: EntryId,
) -> Result<Option<LinkedCommit>, BoardError> {
    super::board_commits::linked_commit_for_entry(conn, entry)
}
