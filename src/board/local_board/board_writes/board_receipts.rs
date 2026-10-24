//! Operation replay keys and current mutation receipts.

use super::super::*;

pub(in crate::board::local_board) fn is_dedupable(op: &BoardOp) -> bool {
    op.is_dedupable()
}

pub(in crate::board::local_board) fn request_dedupe_key(
    request: &BoardRequest,
) -> Result<String, BoardError> {
    request
        .dedupe_key()
        .map(|key| key.to_string())
        .map_err(|error| invalid("board_unavailable", error.to_string()))
}

pub(in crate::board::local_board) fn read_receipt(
    tx: &rusqlite::Transaction<'_>,
    key: BoardRequestKey,
    actor_id: i64,
    now: i64,
) -> Result<BoardReceipt, BoardError> {
    let key_text = key.to_string();
    let stored: Option<String> = tx
        .query_row(
            concat!(
                "SELECT reply_json FROM operation_dedupes ",
                "WHERE dedupe_key=?1 AND actor_id=?2 AND created_at>=?3"
            ),
            rusqlite::params![
                key_text,
                actor_id,
                now.saturating_sub(BOARD_WRITE_RECEIPT_RETENTION_SECS)
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    let Some(stored) = stored else {
        return Ok(unknown_receipt(key));
    };
    let reply: BoardReply = serde_json::from_str(&stored)
        .map_err(|error| invalid("board_unavailable", error.to_string()))?;
    let BoardResult::Change(change) = reply.result else {
        return Ok(unknown_receipt(key));
    };
    Ok(BoardReceipt {
        key,
        state: BoardReceiptState::Committed,
        change: Some(change),
    })
}

fn unknown_receipt(key: BoardRequestKey) -> BoardReceipt {
    BoardReceipt {
        key,
        state: BoardReceiptState::Unknown,
        change: None,
    }
}

pub(in crate::board::local_board) fn receipt_current(
    tx: &Transaction<'_>,
    ctx: &super::super::WriteContext,
    request: &BoardRequest,
    reply: &BoardReply,
) -> Result<bool, BoardError> {
    match &request.op {
        BoardOp::FeedbackTriage { entry, .. } | BoardOp::FeedbackClose { entry, .. } => {
            let BoardResult::Change(change) = &reply.result else {
                return Ok(false);
            };
            // A receipt may be replayed after its grant expires or is revoked,
            // but never for a different report if operation keys collide.
            tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM entries WHERE id=?1 AND supersedes=?2)",
                params![sql_number(change.entry.get()), sql_number(entry.get())],
                |row| row.get(0),
            )
            .map_err(sql_error)
        }
        BoardOp::Post {
            target: BoardRef::Task(task),
            progress,
            verdict,
            ..
        } if progress.is_some() || verdict.is_some() => {
            let BoardResult::Change(change) = &reply.result else {
                return Ok(false);
            };
            structured_task_post_receipt_current(tx, ctx, *task, change.entry, progress, verdict)
        }
        BoardOp::Post {
            progress, verdict, ..
        } if progress.is_some() || verdict.is_some() => Ok(false),
        BoardOp::ClaimTask { delegate, .. } => {
            let holder = match delegate {
                Some(target) => target.holder(&request.actor).map_err(BoardError::from)?,
                None => request.actor.clone(),
            };
            claim_receipt_current(tx, &holder, reply)
        }
        BoardOp::CarveClaim { .. } => claim_receipt_current(tx, &request.actor, reply),
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
        BoardOp::Hello {
            model,
            effort,
            worker,
        } => {
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
            let session_active: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_sessions s JOIN actors a ON a.id=s.actor_id WHERE a.user=?1 AND a.host=?2 AND a.harness=?3 AND a.session=?4 AND s.ended_at IS NULL)",
                params![request.actor.user,request.actor.host,request.actor.harness.as_str(),request.actor.session], |row| row.get(0)
            ).map_err(sql_error)?;
            let worker_active = if let Some(worker) = worker {
                let binding =
                    super::super::board_queries::worker_reads::worker_for_address(tx, worker)?;
                binding.is_some_and(|row| {
                    row.ended_at.is_none() && row.record.actor.as_ref() == Some(&request.actor)
                })
            } else {
                true
            };
            Ok(session_active
                && worker_active
                && current_model.as_deref() == Some(model.as_str())
                && current_effort == *effort)
        }
        BoardOp::Grant { .. } => {
            let BoardResult::Change(change) = &reply.result else {
                return Ok(false);
            };
            // A grant replays only while it still confers the authority it recorded.
            tx.query_row(
                concat!(
                    "SELECT EXISTS(SELECT 1 FROM authority_grants WHERE entry_id=?1 ",
                    "AND revoked_by_entry IS NULL AND (expires_at IS NULL OR expires_at>?2))"
                ),
                params![sql_number(change.entry.get()), ctx.now],
                |row| row.get(0),
            )
            .map_err(sql_error)
        }
        BoardOp::WorkerAdd { .. } => {
            let BoardResult::Change(change) = &reply.result else {
                return Ok(false);
            };
            // A worker replays only while it is unended on the task's unended claim.
            tx.query_row(
                concat!(
                    "SELECT EXISTS(SELECT 1 FROM claim_workers w JOIN claims c ON c.id=w.claim_id ",
                    "WHERE w.entry_id=?1 AND w.ended_at IS NULL AND c.ended_at IS NULL)"
                ),
                [sql_number(change.entry.get())],
                |row| row.get(0),
            )
            .map_err(sql_error)
        }
        _ => Ok(true),
    }
}

fn structured_task_post_receipt_current(
    tx: &Transaction<'_>,
    ctx: &super::super::WriteContext,
    task: TaskId,
    entry: EntryId,
    progress: &Option<ProgressInput>,
    verdict: &Option<ReviewVerdict>,
) -> Result<bool, BoardError> {
    let Some(claim) = super::task_claims::active_claim(tx, task, ctx.now, ctx.claim_ttl_secs)?
    else {
        return Ok(false);
    };
    if claim.record.stale {
        return Ok(false);
    }
    let claim_row: Option<(i64, u64)> = tx
        .query_row(
            concat!(
                "SELECT c.id,e.seq FROM claims c JOIN entries e ON e.id=c.entry_id ",
                "WHERE c.entry_id=?1 AND c.plan_id=?2 AND c.task_ordinal=?3 AND c.ended_at IS NULL"
            ),
            params![
                sql_number(claim.record.entry.get()),
                sql_number(task.plan.get()),
                sql_number(task.ordinal)
            ],
            |row| Ok((row.get(0)?, row_number(row, 1)?)),
        )
        .optional()
        .map_err(sql_error)?;
    let Some((claim_id, claim_seq)) = claim_row else {
        return Ok(false);
    };
    let binding =
        super::super::board_queries::worker_reads::active_worker_binding(tx, ctx.actor_id, task)?;

    if progress.is_some()
        && !progress_receipt_matches(
            tx,
            ctx,
            entry,
            claim.actor_id,
            claim_id,
            claim_seq,
            &binding,
        )?
    {
        return Ok(false);
    }
    if verdict.is_some()
        && !verdict_receipt_matches(
            tx,
            ctx,
            task,
            entry,
            claim.actor_id,
            claim_id,
            claim_seq,
            &binding,
        )?
    {
        return Ok(false);
    }
    Ok(true)
}

fn progress_receipt_matches(
    tx: &Transaction<'_>,
    ctx: &super::super::WriteContext,
    entry: EntryId,
    claim_actor_id: i64,
    claim_id: i64,
    claim_seq: u64,
    binding: &Option<super::super::board_queries::worker_reads::ActiveWorkerBinding>,
) -> Result<bool, BoardError> {
    let row: Option<(i64, Option<i64>, i64, Option<i64>, u64)> = tx
        .query_row(
            concat!(
                "SELECT p.claim_id,p.worker_id,e.actor_id,e.worker_id,e.seq ",
                "FROM progress_facts p JOIN entries e ON e.id=p.entry_id WHERE p.entry_id=?1"
            ),
            [sql_number(entry.get())],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row_number(row, 4)?,
                ))
            },
        )
        .optional()
        .map_err(sql_error)?;
    let Some((stored_claim, fact_worker, entry_actor, entry_worker, entry_seq)) = row else {
        return Ok(false);
    };
    if stored_claim != claim_id
        || fact_worker != entry_worker
        || entry_actor != ctx.actor_id
        || entry_seq <= claim_seq
    {
        return Ok(false);
    }
    Ok(match fact_worker {
        Some(worker_id) => binding
            .as_ref()
            .is_some_and(|current| current.worker_id == worker_id && current.claim_id == claim_id),
        None => claim_actor_id == ctx.actor_id && binding.is_none(),
    })
}

fn verdict_receipt_matches(
    tx: &Transaction<'_>,
    ctx: &super::super::WriteContext,
    task: TaskId,
    entry: EntryId,
    claim_actor_id: i64,
    claim_id: i64,
    claim_seq: u64,
    binding: &Option<super::super::board_queries::worker_reads::ActiveWorkerBinding>,
) -> Result<bool, BoardError> {
    let row: Option<(
        i64,
        Option<i64>,
        u64,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        u64,
    )> = tx
        .query_row(
            concat!(
                "SELECT e.actor_id,e.worker_id,e.seq,cw.claim_id,cw.actor_id,cw.ended_at,e.plan_id ",
                "FROM review_verdicts v JOIN entries e ON e.id=v.entry_id ",
                "LEFT JOIN claim_workers cw ON cw.id=e.worker_id ",
                "WHERE v.entry_id=?1 AND e.kind='review' AND EXISTS(",
                "SELECT 1 FROM entry_refs r WHERE r.entry_id=e.id AND r.target=?2)"
            ),
            params![sql_number(entry.get()), task.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row_number(row, 2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row_number(row, 6)?,
                ))
            },
        )
        .optional()
        .map_err(sql_error)?;
    let Some((
        entry_actor,
        entry_worker,
        entry_seq,
        worker_claim,
        worker_actor,
        worker_ended,
        plan,
    )) = row
    else {
        return Ok(false);
    };
    if plan != task.plan.get() || entry_actor != ctx.actor_id || entry_seq <= claim_seq {
        return Ok(false);
    }
    Ok(match entry_worker {
        Some(worker_id) => {
            worker_claim == Some(claim_id)
                && worker_actor == Some(ctx.actor_id)
                && worker_ended.is_none()
                && binding.as_ref().is_some_and(|current| {
                    current.worker_id == worker_id && current.claim_id == claim_id
                })
        }
        None => claim_actor_id == ctx.actor_id && binding.is_none(),
    })
}

fn claim_receipt_current(
    tx: &Transaction<'_>,
    holder: &BoardActor,
    reply: &BoardReply,
) -> Result<bool, BoardError> {
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
            holder.user,
            holder.host,
            holder.harness.as_str(),
            holder.session
        ],
        |r| r.get(0),
    )
    .map_err(sql_error)
}
