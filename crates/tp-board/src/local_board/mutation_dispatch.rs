//! Serialized mutation dispatch and replay handling.

mod mutation_ops;
#[cfg(test)]
mod tests;

use super::*;

impl LocalBoard {
    /// Answers a bye that needs no write without taking the writer: an identity
    /// with no recorded session, or a session that already ended.
    pub(crate) fn settled_bye(&self, actor: &BoardActor) -> Result<Option<BoardReply>, BoardError> {
        let conn = self.reader.as_ref().unwrap_or(&self.conn);
        let mut reply = board_writes::task_claims::settled_bye(conn, actor)?;
        if let Some(reply) = &mut reply {
            reply.backend = format!("local:{}", self.path.display());
        }
        Ok(reply)
    }

    pub(super) fn dispatch(
        &mut self,
        request: &BoardRequest,
        imported: bool,
    ) -> Result<BoardReply, BoardError> {
        request.validate().map_err(BoardError::from)?;
        if request.op.is_read_only() {
            return self.dispatch_read(request);
        }
        if self.reader.is_none() {
            return Err(invalid(
                BoardErrorCode::InvalidOptions,
                "operation requires writable board storage",
            ));
        }
        #[cfg(unix)]
        if !request.op.is_read_only() {
            use std::os::unix::fs::PermissionsExt;
            let parent = self.path.parent().ok_or_else(|| {
                invalid(BoardErrorCode::BoardUnavailable, "database has no parent")
            })?;
            if parent
                .metadata()
                .map_err(|e| invalid(BoardErrorCode::BoardUnavailable, e.to_string()))?
                .permissions()
                .mode()
                & 0o200
                == 0
            {
                return Err(invalid(
                    BoardErrorCode::BoardUnavailable,
                    "database directory is not writable",
                ));
            }
        }
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(sql_error)?;
        // A bye with nothing to end creates no actor, session, entry, or event.
        if matches!(&request.op, BoardOp::Bye)
            && let Some(mut reply) = board_writes::task_claims::settled_bye(&tx, &request.actor)?
        {
            reply.backend = format!("local:{}", self.path.display());
            return Ok(reply);
        }
        let now = unix_now()?;
        let actor_id = ensure_actor(&tx, &request.actor, now)?;
        if !matches!(&request.op, BoardOp::Bye) {
            tx.execute(
                "UPDATE agent_sessions SET ended_at=NULL WHERE actor_id=?1",
                [actor_id],
            )
            .map_err(sql_error)?;
        }
        if let Some(claims) = &request.claims {
            tx.execute(
                "UPDATE agent_sessions SET model=COALESCE(?2,model),effort=COALESCE(?3,effort) WHERE actor_id=?1",
                params![actor_id, claims.model, claims.effort],
            )
            .map_err(sql_error)?;
        }
        let (model, effort): (Option<String>, Option<String>) = tx
            .query_row(
                "SELECT model,effort FROM agent_sessions WHERE actor_id=?1",
                [actor_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(sql_error)?;
        let seq = max_seq(&tx)?
            .get()
            .checked_add(1)
            .ok_or_else(|| invalid(BoardErrorCode::BoardUnavailable, "event sequence exhausted"))?;
        let key = request_dedupe_key(request)?;
        let ctx = WriteContext {
            actor_id,
            actor: request.actor.clone(),
            model,
            effort,
            now,
            seq: EventSeq::new(seq),
            claim_ttl_secs: self.claim_ttl_secs,
            via: imported.then_some(FeedbackVia::Outbox),
            origin: request.origin.clone(),
            authority: None,
        };
        let dedupable = is_dedupable(&request.op);
        let import_key = match &request.op {
            BoardOp::Feedback { import_key, .. } => import_key.as_ref(),
            _ => None,
        };
        if let Some(import_key) = import_key {
            if let Some(mut reply) =
                board_writes::feedback_entries::imported_reply(&tx, import_key)?
            {
                if let BoardResult::Change(change) = &reply.result {
                    if let Some(plan) = change.plan {
                        board_writes::task_claims::refresh_plan_claims(&tx, actor_id, plan, now)?;
                    }
                }
                reply.backend = format!("local:{}", self.path.display());
                tx.commit().map_err(sql_error)?;
                return Ok(reply);
            }
        }
        if dedupable {
            let stored: Option<String> = tx
                .query_row(
                    "SELECT reply_json FROM operation_dedupes WHERE dedupe_key=?1 AND created_at>=?2",
                    params![key, now - BOARD_WRITE_RECEIPT_RETENTION_SECS],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql_error)?;
            if let Some(stored) = stored {
                let mut reply: BoardReply = serde_json::from_str(&stored)
                    .map_err(|e| invalid(BoardErrorCode::BoardUnavailable, e.to_string()))?;
                // Stored receipts older than the wire API upgrade; validation stays strict.
                if reply.api < BOARD_API {
                    reply.api = BOARD_API;
                }
                if receipt_current(&tx, &ctx, request, &reply)? {
                    let plan = if let BoardResult::Change(change) = &mut reply.result {
                        change.deduplicated = true;
                        if let Some(import_key) = import_key {
                            board_writes::feedback_entries::remember_import(
                                &tx,
                                import_key,
                                change.entry,
                            )?;
                        }
                        change.plan
                    } else {
                        None
                    };
                    if let Some(plan) = plan {
                        refresh_mutation_activity(&tx, &ctx, plan, request, &reply)?;
                    }
                    reply.backend = format!("local:{}", self.path.display());
                    tx.commit().map_err(sql_error)?;
                    return Ok(reply);
                }
            }
        }
        // Own-holder resumes and scope replacements refresh the live claim
        // without an event; predict them so the receipt check expects zero.
        let claim_refresh = claim_refreshes_in_place(&tx, &request.op, actor_id)?;
        let mut reply = mutation_ops::apply_mutation(&tx, &ctx, request)?;
        #[cfg(test)]
        if std::mem::take(&mut self.panic_after_write) {
            panic!("injected panic after uncommitted board mutation");
        }
        if dedupable {
            let plan = match &reply.result {
                BoardResult::Change(change) => change.plan,
                _ => request.op.plan_id(),
            };
            if let Some(plan) = plan {
                refresh_mutation_activity(&tx, &ctx, plan, request, &reply)?;
            }
            let events: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM events WHERE seq=?1",
                    [sql_number(ctx.seq.get())],
                    |r| r.get(0),
                )
                .map_err(sql_error)?;
            let replay =
                matches!(&reply.result, BoardResult::Change(change) if change.deduplicated);
            let expected = if claim_refresh { 0 } else { i64::from(!replay) };
            if events != expected {
                return Err(invalid(
                    BoardErrorCode::BoardUnavailable,
                    if claim_refresh {
                        "claim refresh must not produce an event"
                    } else {
                        "mutation did not produce exactly one event"
                    },
                ));
            }
            reply.backend = format!("local:{}", self.path.display());
            let encoded = serde_json::to_string(&reply)
                .map_err(|e| invalid(BoardErrorCode::BoardUnavailable, e.to_string()))?;
            tx.execute(
                "DELETE FROM operation_dedupes WHERE created_at<?1",
                [now - BOARD_WRITE_RECEIPT_RETENTION_SECS],
            )
            .map_err(sql_error)?;
            tx.execute(
                concat!(
                    "INSERT INTO operation_dedupes(dedupe_key,reply_json,created_at,actor_id) VALUES(?1,?2,?3,?4) ",
                    "ON CONFLICT(dedupe_key) DO UPDATE SET reply_json=excluded.reply_json,",
                    "created_at=excluded.created_at,",
                    "actor_id=COALESCE(operation_dedupes.actor_id,excluded.actor_id)"
                ),
                params![key, encoded, now, actor_id],
            )
            .map_err(sql_error)?;
        }
        reply.backend = format!("local:{}", self.path.display());
        if matches!(request.op, BoardOp::Inbox { .. }) {
            reply.snapshot_seq = Some(max_seq(&tx)?);
        }
        tx.commit().map_err(sql_error)?;
        Ok(reply)
    }
}

fn refresh_mutation_activity(
    tx: &Transaction<'_>,
    ctx: &WriteContext,
    plan: PlanId,
    request: &BoardRequest,
    reply: &BoardReply,
) -> Result<(), BoardError> {
    let Some(task) = task_post_target(request) else {
        return board_writes::task_claims::refresh_plan_claims(tx, ctx.actor_id, plan, ctx.now);
    };
    let binding = board_queries::worker_reads::active_worker_binding(tx, ctx.actor_id, task)?;
    if let Some(binding) = binding {
        tx.execute(
            "UPDATE claim_workers SET last_active=max(last_active,?3) WHERE id=?1 AND claim_id=?2 AND ended_at IS NULL",
            params![binding.worker_id, binding.claim_id, ctx.now],
        )
        .map_err(sql_error)?;
        tx.execute(
            "UPDATE claims SET last_active=max(last_active,?2) WHERE id=?1 AND ended_at IS NULL",
            params![binding.claim_id, ctx.now],
        )
        .map_err(sql_error)?;
        return Ok(());
    }

    if let BoardResult::Change(change) = &reply.result {
        let worker_id: Option<Option<i64>> = tx
            .query_row(
                "SELECT worker_id FROM entries WHERE id=?1",
                [sql_number(change.entry.get())],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_error)?;
        if worker_id.is_none() || worker_id.flatten().is_some() {
            return Ok(());
        }
    }
    board_writes::task_claims::refresh_plan_claims(tx, ctx.actor_id, plan, ctx.now)
}

fn task_post_target(request: &BoardRequest) -> Option<TaskId> {
    match &request.op {
        BoardOp::Post {
            target: BoardRef::Task(task),
            ..
        } => Some(*task),
        _ => None,
    }
}

/// Whether a resume or scope replacement targets the caller's own live lease,
/// which `claim_task` refreshes in place without writing an event.
fn claim_refreshes_in_place(
    tx: &Transaction<'_>,
    op: &BoardOp,
    actor_id: i64,
) -> Result<bool, BoardError> {
    let BoardOp::ClaimTask {
        task,
        scope,
        resume,
        delegate,
    } = op
    else {
        return Ok(false);
    };
    if (!resume.is_resuming() && scope.is_none()) || delegate.is_some() {
        return Ok(false);
    }
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM claims WHERE plan_id=?1 AND task_ordinal=?2 AND actor_id=?3 AND ended_at IS NULL)",
        params![
            sql_number(task.plan.get()),
            sql_number(task.ordinal),
            actor_id
        ],
        |row| row.get(0),
    )
    .map_err(sql_error)
}
