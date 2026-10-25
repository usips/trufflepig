//! Recipient SQL shared by event, attention, and reminder readers.

use crate::board_protocol::BoardErrorCode;
use rusqlite::Row;

use crate::board_ids::EntryId;
use crate::board_protocol::AuthoritySource;
use crate::local_board::{BoardError, invalid, sql_error, sqlite_u64};
use std::net::IpAddr;
use tp_core::request_context::ActorOrigin;
use tp_core::request_context::HarnessLabel;

pub(in crate::local_board) fn actor_origin_from_row(
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
            BoardErrorCode::BoardUnavailable,
            "stored peer IP has no origin identity",
        )),
        (Some(harness), Some(session)) => {
            let peer_ip = peer_ip
                .map(|value| {
                    value.parse::<IpAddr>().map_err(|error| {
                        invalid(
                            BoardErrorCode::BoardUnavailable,
                            format!("invalid stored origin peer IP: {error}"),
                        )
                    })
                })
                .transpose()?;
            Ok(Some(ActorOrigin {
                harness: HarnessLabel::parse(&harness).map_err(|error| {
                    invalid(BoardErrorCode::BoardUnavailable, error.to_string())
                })?,
                session,
                peer_ip,
            }))
        }
        _ => Err(invalid(
            BoardErrorCode::BoardUnavailable,
            "stored origin has only one identity component",
        )),
    }
}

pub(in crate::local_board) fn authority_from_row(
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
                .map_err(|error| invalid(BoardErrorCode::BoardUnavailable, error.to_string()))
        }
        (Some(basis), None) => AuthoritySource::parse(basis)
            .map(Some)
            .map_err(|error| invalid(BoardErrorCode::BoardUnavailable, error.to_string())),
        _ => Err(invalid(
            BoardErrorCode::BoardUnavailable,
            "stored authority has an invalid grant reference",
        )),
    }
}

/// Matches explicit actor recipients and task recipients bound to an unended claim.
pub(in crate::local_board) fn addressed_to_actor_sql(
    recipient: &str,
    user: &str,
    harness: &str,
    identity: &str,
) -> String {
    let task_successors = task_successors_cte(&format!(
        "SELECT task_source.plan_id,task_source.ordinal FROM tasks task_source \
         WHERE {recipient}='P'||task_source.plan_id||'.'||task_source.ordinal"
    ));
    format!(
        concat!(
            "({recipient} IN ({user},{harness},{identity}) OR ",
            "EXISTS({task_successors} SELECT 1 FROM task_successor current ",
            "JOIN claims task_claim ON task_claim.plan_id=current.plan_id ",
            "AND task_claim.task_ordinal=current.ordinal ",
            "JOIN actors holder ON holder.id=task_claim.actor_id ",
            "WHERE task_claim.ended_at IS NULL AND ",
            "holder.user||'@'||holder.host||'/'||holder.harness||'/'||holder.session={identity}) OR ",
            "EXISTS({task_successors} SELECT 1 FROM task_successor current ",
            "JOIN claims task_claim ON task_claim.plan_id=current.plan_id ",
            "AND task_claim.task_ordinal=current.ordinal ",
            "JOIN claim_workers task_worker ON task_claim.id=task_worker.claim_id ",
            "JOIN actors worker ON worker.id=task_worker.actor_id ",
            "WHERE task_claim.ended_at IS NULL AND task_worker.ended_at IS NULL AND ",
            "worker.user||'@'||worker.host||'/'||worker.harness||'/'||worker.session={identity}))"
        ),
        recipient = recipient,
        user = user,
        harness = harness,
        identity = identity,
        task_successors = task_successors,
    )
}

fn task_successors_cte(anchor: &str) -> String {
    format!(
        concat!(
            "WITH RECURSIVE task_successor(plan_id,ordinal) AS ({anchor} ",
            "UNION SELECT transfer.destination_plan,transfer.destination_ordinal ",
            "FROM task_transfers transfer JOIN task_successor current ",
            "ON transfer.source_plan=current.plan_id AND transfer.source_ordinal=current.ordinal)"
        ),
        anchor = anchor
    )
}

/// Matches entries addressed to a task or linked to it by reference.
pub(in crate::local_board) fn task_entry_sql(entry_id: &str) -> String {
    format!(
        concat!(
            "(EXISTS(SELECT 1 FROM entry_refs task_reference JOIN tasks task_target ",
            "ON task_reference.target='P'||task_target.plan_id||'.'||task_target.ordinal ",
            "WHERE task_reference.entry_id={entry_id}) OR ",
            "EXISTS(SELECT 1 FROM entries addressed JOIN tasks task_target ",
            "ON addressed.to_whom='P'||task_target.plan_id||'.'||task_target.ordinal ",
            "WHERE addressed.id={entry_id}))"
        ),
        entry_id = entry_id,
    )
}

/// Lets the owning user's human web session see any task-referenced question.
pub(in crate::local_board) fn task_owner_human_question_sql(
    entry_id: &str,
    user: &str,
    harness: &str,
) -> String {
    let task_successors = task_successors_cte(&format!(
        concat!(
            "SELECT task_target.plan_id,task_target.ordinal FROM entry_refs task_reference ",
            "JOIN tasks task_target ON task_reference.target='P'||task_target.plan_id||'.'||task_target.ordinal ",
            "WHERE task_reference.entry_id={entry_id} UNION ",
            "SELECT task_target.plan_id,task_target.ordinal FROM entries addressed ",
            "JOIN tasks task_target ON addressed.to_whom='P'||task_target.plan_id||'.'||task_target.ordinal ",
            "WHERE addressed.id={entry_id}"
        ),
        entry_id = entry_id
    ));
    format!(
        concat!(
            "EXISTS({task_successors} SELECT 1 FROM task_successor current ",
            "JOIN plans task_plan ON task_plan.id=current.plan_id ",
            "WHERE NOT EXISTS(SELECT 1 FROM task_transfers outgoing ",
            "WHERE outgoing.source_plan=current.plan_id AND outgoing.source_ordinal=current.ordinal) ",
            "AND task_plan.owner_user={user} AND {harness}='human')"
        ),
        user = user,
        harness = harness,
        task_successors = task_successors,
    )
}

/// Matches an event generated by a still-open task question.
pub(in crate::local_board) fn event_task_question_sql(event: &str) -> String {
    let task_entry = task_entry_sql("task_question.id");
    format!(
        concat!(
            "EXISTS(SELECT 1 FROM entries task_question ",
            "WHERE task_question.seq={event}.seq AND task_question.kind='question' ",
            "AND {task_entry} ",
            "AND NOT EXISTS(SELECT 1 FROM entries answer JOIN entry_refs answer_ref ",
            "ON answer_ref.entry_id=answer.id WHERE answer.kind='answer' ",
            "AND answer_ref.target='E'||task_question.id) ",
            "AND NOT EXISTS(SELECT 1 FROM entries correction ",
            "WHERE correction.supersedes=task_question.id))"
        ),
        event = event,
        task_entry = task_entry,
    )
}

/// Matches an open task-question event for its plan owner's human session.
pub(in crate::local_board) fn event_task_owner_human_sql(
    event: &str,
    user: &str,
    harness: &str,
) -> String {
    let task_owner = task_owner_human_question_sql("task_question.id", user, harness);
    format!(
        concat!(
            "EXISTS(SELECT 1 FROM entries task_question ",
            "WHERE task_question.seq={event}.seq AND task_question.kind='question' ",
            "AND {task_owner} ",
            "AND NOT EXISTS(SELECT 1 FROM entries answer JOIN entry_refs answer_ref ",
            "ON answer_ref.entry_id=answer.id WHERE answer.kind='answer' ",
            "AND answer_ref.target='E'||task_question.id) ",
            "AND NOT EXISTS(SELECT 1 FROM entries correction ",
            "WHERE correction.supersedes=task_question.id))"
        ),
        event = event,
        task_owner = task_owner,
    )
}

/// Matches a worker-lifecycle event to the session bound to its address. A `worker_end` stays
/// visible to that session after the worker or its claim has closed.
pub(in crate::local_board) fn worker_event_to_actor_sql(
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
            "AND ({event}.kind='worker_end' OR (task_claim.ended_at IS NULL AND task_worker.ended_at IS NULL)) ",
            "AND worker.user||'@'||worker.host||'/'||worker.harness||'/'||worker.session={identity} ",
            "AND worker.user={user} AND worker.harness={harness})"
        ),
        event = event,
        user = user,
        harness = harness,
        identity = identity,
    )
}
