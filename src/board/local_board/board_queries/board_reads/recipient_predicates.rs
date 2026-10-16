//! Recipient SQL shared by event, attention, and reminder readers.

use rusqlite::Row;

use crate::board::board_actor::HarnessLabel;
use crate::board::board_ids::EntryId;
use crate::board::board_protocol::{ActorOrigin, AuthoritySource};
use crate::board::local_board::{BoardError, invalid, sql_error, sqlite_u64};
use std::net::IpAddr;

pub(in crate::board::local_board) fn actor_origin_from_row(
    row: &Row<'_>,
    harness_index: usize,
    session_index: usize,
    peer_ip_index: usize,
) -> Result<Option<ActorOrigin>, BoardError> {
    let harness: Option<String> = row.get(harness_index).map_err(sql_error)?;
    let session: Option<String> = row.get(session_index).map_err(sql_error)?;
    let peer_ip: Option<String> = row.get(peer_ip_index).map_err(sql_error)?;
    match (harness, session) {
        (None, None) if peer_ip.is_none() => Ok(None),
        (None, None) => Err(invalid(
            "board_unavailable",
            "stored peer IP has no origin identity",
        )),
        (Some(harness), Some(session)) => {
            let peer_ip = peer_ip
                .map(|value| {
                    value.parse::<IpAddr>().map_err(|error| {
                        invalid(
                            "board_unavailable",
                            format!("invalid stored origin peer IP: {error}"),
                        )
                    })
                })
                .transpose()?;
            Ok(Some(ActorOrigin {
                harness: HarnessLabel::parse(&harness)
                    .map_err(|error| invalid("board_unavailable", error.to_string()))?,
                session,
                peer_ip,
            }))
        }
        _ => Err(invalid(
            "board_unavailable",
            "stored origin has only one identity component",
        )),
    }
}

pub(in crate::board::local_board) fn authority_from_row(
    row: &Row<'_>,
    basis_index: usize,
    grant_entry_index: usize,
) -> Result<Option<AuthoritySource>, BoardError> {
    let basis: Option<String> = row.get(basis_index).map_err(sql_error)?;
    let grant_entry: Option<i64> = row.get(grant_entry_index).map_err(sql_error)?;
    match (basis.as_deref(), grant_entry) {
        (None, None) => Ok(None),
        (Some("grant"), Some(entry)) => {
            let entry = EntryId::new(sqlite_u64(entry)?).map_err(BoardError::from)?;
            AuthoritySource::parse(&format!("grant:{entry}"))
                .map(Some)
                .map_err(|error| invalid("board_unavailable", error.to_string()))
        }
        (Some(basis), None) => AuthoritySource::parse(basis)
            .map(Some)
            .map_err(|error| invalid("board_unavailable", error.to_string())),
        _ => Err(invalid(
            "board_unavailable",
            "stored authority has an invalid grant reference",
        )),
    }
}

/// Matches explicit actor recipients and task recipients bound to an unended claim.
pub(in crate::board::local_board) fn addressed_to_actor_sql(
    recipient: &str,
    user: &str,
    harness: &str,
    identity: &str,
) -> String {
    format!(
        concat!(
            "({recipient} IN ({user},{harness},{identity}) OR ",
            "EXISTS(SELECT 1 FROM claims task_claim JOIN actors holder ON holder.id=task_claim.actor_id ",
            "WHERE {recipient}='P'||task_claim.plan_id||'.'||task_claim.task_ordinal ",
            "AND task_claim.ended_at IS NULL AND ",
            "holder.user||'@'||holder.host||'/'||holder.harness||'/'||holder.session={identity}) OR ",
            "EXISTS(SELECT 1 FROM claim_workers task_worker JOIN claims task_claim ",
            "ON task_claim.id=task_worker.claim_id JOIN actors worker ON worker.id=task_worker.actor_id ",
            "WHERE {recipient}='P'||task_claim.plan_id||'.'||task_claim.task_ordinal ",
            "AND task_claim.ended_at IS NULL AND task_worker.ended_at IS NULL AND ",
            "worker.user||'@'||worker.host||'/'||worker.harness||'/'||worker.session={identity}))"
        ),
        recipient = recipient,
        user = user,
        harness = harness,
        identity = identity,
    )
}

/// Matches entries whose persisted references identify a task.
pub(in crate::board::local_board) fn task_entry_sql(entry_id: &str) -> String {
    format!(
        concat!(
            "EXISTS(SELECT 1 FROM entry_refs task_reference JOIN tasks task_target ",
            "ON task_reference.target='P'||task_target.plan_id||'.'||task_target.ordinal ",
            "WHERE task_reference.entry_id={entry_id})"
        ),
        entry_id = entry_id,
    )
}

/// Lets the owning user's human web session see any task-referenced question.
pub(in crate::board::local_board) fn task_owner_human_question_sql(
    entry_id: &str,
    user: &str,
    harness: &str,
) -> String {
    format!(
        concat!(
            "EXISTS(SELECT 1 FROM entry_refs task_reference JOIN tasks task_target ",
            "ON task_reference.target='P'||task_target.plan_id||'.'||task_target.ordinal ",
            "JOIN plans task_plan ON task_plan.id=task_target.plan_id ",
            "WHERE task_reference.entry_id={entry_id} AND task_plan.owner_user={user} ",
            "AND {harness}='human')"
        ),
        entry_id = entry_id,
        user = user,
        harness = harness,
    )
}

/// Matches an event generated by a still-open task question.
pub(in crate::board::local_board) fn event_task_question_sql(event: &str) -> String {
    format!(
        concat!(
            "EXISTS(SELECT 1 FROM entries task_question JOIN entry_refs task_reference ",
            "ON task_reference.entry_id=task_question.id JOIN tasks task_target ",
            "ON task_reference.target='P'||task_target.plan_id||'.'||task_target.ordinal ",
            "WHERE task_question.seq={event}.seq AND task_question.kind='question' ",
            "AND NOT EXISTS(SELECT 1 FROM entries answer JOIN entry_refs answer_ref ",
            "ON answer_ref.entry_id=answer.id WHERE answer.kind='answer' ",
            "AND answer_ref.target='E'||task_question.id) ",
            "AND NOT EXISTS(SELECT 1 FROM entries correction ",
            "WHERE correction.supersedes=task_question.id))"
        ),
        event = event,
    )
}

/// Matches an open task-question event for its plan owner's human session.
pub(in crate::board::local_board) fn event_task_owner_human_sql(
    event: &str,
    user: &str,
    harness: &str,
) -> String {
    format!(
        concat!(
            "EXISTS(SELECT 1 FROM entries task_question JOIN entry_refs task_reference ",
            "ON task_reference.entry_id=task_question.id JOIN tasks task_target ",
            "ON task_reference.target='P'||task_target.plan_id||'.'||task_target.ordinal ",
            "JOIN plans task_plan ON task_plan.id=task_target.plan_id ",
            "WHERE task_question.seq={event}.seq AND task_question.kind='question' ",
            "AND task_plan.owner_user={user} AND {harness}='human' ",
            "AND NOT EXISTS(SELECT 1 FROM entries answer JOIN entry_refs answer_ref ",
            "ON answer_ref.entry_id=answer.id WHERE answer.kind='answer' ",
            "AND answer_ref.target='E'||task_question.id) ",
            "AND NOT EXISTS(SELECT 1 FROM entries correction ",
            "WHERE correction.supersedes=task_question.id))"
        ),
        event = event,
        user = user,
        harness = harness,
    )
}

/// Matches a worker-lifecycle event to the session currently bound to its address.
pub(in crate::board::local_board) fn worker_event_to_actor_sql(
    event: &str,
    user: &str,
    harness: &str,
    identity: &str,
) -> String {
    format!(
        concat!(
            "EXISTS(SELECT 1 FROM claim_workers task_worker JOIN claims task_claim ",
            "ON task_claim.id=task_worker.claim_id JOIN actors worker ON worker.id=task_worker.actor_id ",
            "WHERE {event}.subject='P'||task_worker.plan_id||'.'||task_worker.task_ordinal||'/'||task_worker.handle ",
            "AND task_claim.ended_at IS NULL AND task_worker.ended_at IS NULL ",
            "AND worker.user||'@'||worker.host||'/'||worker.harness||'/'||worker.session={identity} ",
            "AND worker.user={user} AND worker.harness={harness})"
        ),
        event = event,
        user = user,
        harness = harness,
        identity = identity,
    )
}
