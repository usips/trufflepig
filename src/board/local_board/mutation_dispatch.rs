//! Serialized mutation dispatch and replay handling.

use super::*;

impl LocalBoard {
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
                "invalid_options",
                "operation requires writable board storage",
            ));
        }
        #[cfg(unix)]
        if !request.op.is_read_only() {
            use std::os::unix::fs::PermissionsExt;
            let parent = self
                .path
                .parent()
                .ok_or_else(|| invalid("board_unavailable", "database has no parent"))?;
            if parent
                .metadata()
                .map_err(|e| invalid("board_unavailable", e.to_string()))?
                .permissions()
                .mode()
                & 0o200
                == 0
            {
                return Err(invalid(
                    "board_unavailable",
                    "database directory is not writable",
                ));
            }
        }
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(sql_error)?;
        let now = unix_now()?;
        let actor_id = ensure_actor(&tx, &request.actor, now)?;
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
            .ok_or_else(|| invalid("board_unavailable", "event sequence exhausted"))?;
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
        };
        let dedupable = is_dedupable(&request.op);
        let import_key = match &request.op {
            BoardOp::Feedback { import_key, .. } => import_key.as_ref(),
            _ => None,
        };
        if let Some(import_key) = import_key {
            if let Some(mut reply) = board_writes::feedback_entries::imported_reply(&tx, import_key)? {
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
                    params![key, now - 600],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql_error)?;
            if let Some(stored) = stored {
                let mut reply: BoardReply = serde_json::from_str(&stored)
                    .map_err(|e| invalid("board_unavailable", e.to_string()))?;
                // Stored receipts older than the wire API upgrade; validation stays strict.
                if reply.api < BOARD_API {
                    reply.api = BOARD_API;
                }
                if receipt_current(&tx, request, &reply)? {
                    if let BoardResult::Change(change) = &mut reply.result {
                        change.deduplicated = true;
                        if let Some(import_key) = import_key {
                            board_writes::feedback_entries::remember_import(&tx, import_key, change.entry)?;
                        }
                        if let Some(plan) = change.plan {
                            board_writes::task_claims::refresh_plan_claims(&tx, actor_id, plan, now)?;
                        }
                    }
                    reply.backend = format!("local:{}", self.path.display());
                    tx.commit().map_err(sql_error)?;
                    return Ok(reply);
                }
            }
        }
        let mut reply = match &request.op {
            BoardOp::Hello { model, effort } => {
                board_writes::entry_writes::hello(&tx, &ctx, model, effort.as_deref())?
            }
            BoardOp::Inbox {
                after,
                limit,
                repo_key,
                all,
            } => board_feed::inbox(&tx, &ctx, *after, *limit, repo_key.as_ref(), *all)?,
            BoardOp::AcknowledgeInbox { rendered_through } => {
                board_feed::acknowledge(&tx, &ctx, *rendered_through)?
            }
            BoardOp::New {
                title,
                body,
                steward,
                repo_key,
            } => board_writes::plan_writes::new_plan(&tx, &ctx, title, body, steward.as_ref(), repo_key.as_ref())?,
            BoardOp::Post {
                target,
                kind,
                body,
                to,
                supersedes,
            } => board_writes::entry_writes::post(&tx, &ctx, target, *kind, body, to.as_ref(), *supersedes)?,
            BoardOp::TaskCreate {
                plan,
                title,
                to,
                section,
            } => {
                board_writes::task_writes::create_task(&tx, &ctx, *plan, title, to.as_ref(), section.as_deref())?
            }
            BoardOp::TaskMove { task, column, to } => {
                board_writes::task_writes::move_task(&tx, &ctx, *task, *column, to.as_ref())?
            }
            BoardOp::ClaimTask {
                task,
                scope,
                resume,
                delegate,
            } => board_writes::task_claims::claim_task(
                &tx,
                &ctx,
                *task,
                scope.as_ref(),
                *resume,
                delegate.as_ref(),
            )?,
            BoardOp::CarveClaim {
                plan,
                title,
                scope,
                section,
            } => board_writes::task_claims::carve_claim(&tx, &ctx, *plan, title, scope, section.as_deref())?,
            BoardOp::Propose {
                base,
                body,
                summary,
                supersedes,
            } => board_writes::plan_writes::propose(&tx, &ctx, *base, body, summary, *supersedes)?,
            BoardOp::Edit {
                base,
                body,
                summary,
            } => board_writes::plan_writes::edit(&tx, &ctx, *base, body, summary)?,
            BoardOp::Accept { proposal, note } => {
                board_writes::plan_writes::accept(&tx, &ctx, *proposal, note.as_ref())?
            }
            BoardOp::Reject { proposal, reason } => {
                board_writes::plan_writes::reject(&tx, &ctx, *proposal, reason)?
            }
            BoardOp::Feedback { .. } => board_writes::feedback_entries::write_feedback(&tx, &ctx, &request.op)?,
            BoardOp::FeedbackTriage { .. } | BoardOp::FeedbackClose { .. } => {
                board_writes::feedback_entries::close_feedback(&tx, &ctx, &request.op)?
            }
            BoardOp::RegisterRepo { registration } => {
                board_writes::entry_writes::register_repo(&tx, &ctx, registration)?
            }
            BoardOp::Show { .. }
            | BoardOp::Search { .. }
            | BoardOp::Review { .. }
            | BoardOp::FeedbackList { .. }
            | BoardOp::Repositories { .. }
            | BoardOp::Overview { .. }
            | BoardOp::Attention { .. }
            | BoardOp::Feed { .. }
            | BoardOp::History { .. }
            | BoardOp::Entries { .. }
            | BoardOp::Tasks { .. }
            | BoardOp::Claims { .. } => unreachable!("read operations use query-only dispatch"),
            BoardOp::RecordScan {
                repo_key,
                host,
                common_dir,
                error,
            } => board_writes::entry_writes::record_scan(&tx, repo_key, host, common_dir, error.as_deref())?,
            BoardOp::ForgetRepoPath {
                repo_key,
                host,
                common_dir,
            } => board_writes::entry_writes::forget_repo_path(&tx, &ctx, repo_key, host, common_dir)?,
            BoardOp::LinkCommits { commits } => board_writes::entry_writes::link_commits(&tx, &ctx, commits)?,
            BoardOp::LinkCommit {
                task, resolution, ..
            } => {
                let commit = resolution.as_deref().ok_or_else(|| {
                    invalid(
                        "invalid_options",
                        "commit links resolve through the board host",
                    )
                })?;
                board_writes::entry_writes::link_commit(&tx, &ctx, *task, commit)?
            }
        };
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
                board_writes::task_claims::refresh_plan_claims(&tx, actor_id, plan, now)?;
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
            if events != i64::from(!replay) {
                return Err(invalid(
                    "board_unavailable",
                    "mutation did not produce exactly one event",
                ));
            }
            reply.backend = format!("local:{}", self.path.display());
            let encoded = serde_json::to_string(&reply)
                .map_err(|e| invalid("board_unavailable", e.to_string()))?;
            tx.execute(
                "DELETE FROM operation_dedupes WHERE created_at<?1",
                [now - 600],
            )
            .map_err(sql_error)?;
            tx.execute(
                concat!(
                    "INSERT INTO operation_dedupes(dedupe_key,reply_json,created_at) VALUES(?1,?2,?3) ",
                    "ON CONFLICT(dedupe_key) DO UPDATE SET reply_json=excluded.reply_json,",
                    "created_at=excluded.created_at"
                ),
                params![key, encoded, now],
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
