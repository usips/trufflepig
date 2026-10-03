//! Query-only board collections; callers keep all reads in one deferred transaction.

mod action_collection_reads;
mod event_collection_reads;
mod feedback_collection_reads;
#[cfg(test)]
mod tests;

pub(super) use action_collection_reads::{attention, overview};
pub(super) use event_collection_reads::{entries_page, feed, history};
pub(super) use feedback_collection_reads::{feedback_page, feedback_record};

use rusqlite::{Connection, Params};

use super::{BoardError, WriteContext, invalid, max_seq, sql_error};
use crate::board::board_ids::EventSeq;

#[cfg(test)]
use crate::board::board_actor::HarnessLabel;
#[cfg(test)]
use crate::board::board_domain::board_collections::{
    AttentionReply, EntriesPage, EntryCursor, FeedbackPage,
};
#[cfg(test)]
use crate::board::board_ids::{EntryId, PlanId, PlanRevision, RepoKey, TaskId};
#[cfg(test)]
use crate::board::board_protocol::{BoardResult, RevisionSource};
#[cfg(test)]
use crate::board::board_vocabulary::{EntryKind, FeedbackKind};
#[cfg(test)]
use rusqlite::params;

const COLLECTION_LIMIT: usize = 200;

pub(super) fn validate_limit(limit: usize, maximum: usize) -> Result<(), BoardError> {
    if !(1..=maximum).contains(&limit) {
        return Err(invalid(
            "invalid_options",
            format!("collection limit must be 1..{maximum}"),
        ));
    }
    Ok(())
}

pub(super) fn sequence_window(
    conn: &Connection,
    after: Option<EventSeq>,
    through: Option<EventSeq>,
) -> Result<(EventSeq, EventSeq), BoardError> {
    let snapshot = max_seq(conn)?;
    let through = through.unwrap_or(snapshot);
    let after = after.unwrap_or_default();
    if through > snapshot {
        return Err(invalid(
            "invalid_reference",
            "through exceeds the current snapshot",
        ));
    }
    if after > through {
        return Err(invalid("invalid_reference", "after exceeds through"));
    }
    Ok((after, through))
}

pub(super) fn count(
    conn: &Connection,
    sql: &str,
    parameters: impl Params,
) -> Result<usize, BoardError> {
    let total: i64 = conn
        .query_row(sql, parameters, |row| row.get(0))
        .map_err(sql_error)?;
    usize::try_from(total).map_err(|_| invalid("board_unavailable", "invalid collection count"))
}

pub(super) fn claim_ttl(ctx: &WriteContext) -> u64 {
    ctx.claim_ttl_secs.max(0) as u64
}
