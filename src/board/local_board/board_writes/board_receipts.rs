//! Operation replay keys and current mutation receipts.

use super::super::*;

pub(in crate::board::local_board) fn is_dedupable(op: &BoardOp) -> bool {
    matches!(
        op,
        BoardOp::Hello { .. }
            | BoardOp::New { .. }
            | BoardOp::Post { .. }
            | BoardOp::TaskCreate { .. }
            | BoardOp::TaskMove { .. }
            | BoardOp::ClaimTask { .. }
            | BoardOp::CarveClaim { .. }
            | BoardOp::Propose { .. }
            | BoardOp::Edit { .. }
            | BoardOp::Accept { .. }
            | BoardOp::Reject { .. }
            | BoardOp::Feedback { .. }
            | BoardOp::FeedbackTriage { .. }
            | BoardOp::FeedbackClose { .. }
    )
}

pub(in crate::board::local_board) fn request_dedupe_key(request: &BoardRequest) -> Result<String, BoardError> {
    if let BoardOp::Feedback {
        kind,
        summary,
        body,
        plan,
        ..
    } = &request.op
    {
        let bytes = serde_json::to_vec(&(kind, summary, body, plan))
            .map_err(|error| invalid("board_unavailable", error.to_string()))?;
        return Ok(blake3::hash(&bytes).to_hex().to_string());
    }
    let canonical = request.clone();
    let bytes =
        serde_json::to_vec(&canonical).map_err(|e| invalid("board_unavailable", e.to_string()))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

pub(in crate::board::local_board) fn receipt_current(
    tx: &Transaction<'_>,
    request: &BoardRequest,
    reply: &BoardReply,
) -> Result<bool, BoardError> {
    match &request.op {
        BoardOp::ClaimTask { .. } | BoardOp::CarveClaim { .. } => {
            let BoardResult::Change(change) = &reply.result else {
                return Ok(false);
            };
            let Some(task) = change.task else {
                return Ok(false);
            };
            tx.query_row(
                concat!(
                    "SELECT EXISTS(SELECT 1 FROM claims c JOIN actors a ON a.id=c.actor_id WHERE c.plan_id=?1 ",
                    "AND c.task_ordinal=?2 AND c.entry_id=?3 AND c.ended_at IS NULL ",
                    "AND a.user=?4 AND a.host=?5 AND a.harness=?6 AND a.session=?7)"
                ),
                params![
                    sql_number(task.plan.get()),
                    sql_number(task.ordinal),
                    sql_number(change.entry.get()),
                    request.actor.user,
                    request.actor.host,
                    request.actor.harness.as_str(),
                    request.actor.session
                ],
                |r| r.get(0),
            )
            .map_err(sql_error)
        }
        BoardOp::TaskMove { task, .. } => {
            let BoardResult::Change(change) = &reply.result else {
                return Ok(false);
            };
            tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM tasks WHERE plan_id=?1 AND ordinal=?2 AND seq=?3)",
                params![
                    sql_number(task.plan.get()),
                    sql_number(task.ordinal),
                    sql_number(change.seq.get())
                ],
                |r| r.get(0),
            )
            .map_err(sql_error)
        }
        BoardOp::Hello { model, effort } => {
            let (current_model, current_effort): (Option<String>, Option<String>) = tx
                .query_row(
                    concat!(
                        "SELECT s.model,s.effort FROM agent_sessions s JOIN actors a ON a.id=s.actor_id ",
                        "WHERE a.user=?1 AND a.host=?2 AND a.harness=?3 AND a.session=?4"
                    ),
                    params![
                        request.actor.user,
                        request.actor.host,
                        request.actor.harness.as_str(),
                        request.actor.session
                    ],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(sql_error)?;
            Ok(current_model.as_deref() == Some(model.as_str()) && current_effort == *effort)
        }
        _ => Ok(true),
    }
}
