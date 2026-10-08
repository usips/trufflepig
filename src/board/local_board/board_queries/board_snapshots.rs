//! Committed read snapshots and query-only dispatch.

use super::super::*;
use super::{board_reads, board_search, collection_nested, collection_reads};

impl LocalBoard {
    /// Returns one committed event snapshot without retaining a read transaction.
    pub fn read_event_batch(
        &mut self,
        after: EventSeq,
        plan: Option<PlanId>,
        limit: usize,
    ) -> Result<(EventSeq, Vec<EventRecord>), BoardError> {
        let tx = self
            .reader
            .as_mut()
            .unwrap_or(&mut self.conn)
            .transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)
            .map_err(sql_error)?;
        if let Some(plan) = plan {
            require_plan(&tx, plan)?;
        }
        let latest = max_seq(&tx)?;
        let events = board_feed::read_events(&tx, after, latest, plan, &ReadScope::All, limit)?;
        tx.commit().map_err(sql_error)?;
        Ok((latest, events))
    }

    pub(in crate::board::local_board) fn dispatch_read(
        &mut self,
        request: &BoardRequest,
    ) -> Result<BoardReply, BoardError> {
        let reader = self.reader.as_mut().unwrap_or(&mut self.conn);
        let tx = reader
            .transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)
            .map_err(sql_error)?;
        let actor_id = lookup_actor(&tx, &request.actor)?.unwrap_or(-1);
        let ctx = WriteContext {
            actor_id,
            actor: request.actor.clone(),
            model: None,
            effort: None,
            now: unix_now()?,
            seq: EventSeq::new(0),
            claim_ttl_secs: self.claim_ttl_secs,
            via: None,
        };
        let mut reply = match &request.op {
            BoardOp::Inbox {
                after,
                limit,
                scope,
            } => board_feed::inbox(&tx, &ctx, *after, *limit, scope)?,
            BoardOp::Show { target } => board_reads::show(&tx, &ctx, target)?,
            BoardOp::Search { query, plan, limit } => {
                board_search::search(&tx, query, *plan, *limit)?
            }
            BoardOp::Review { base, agent } => {
                board_reads::review(&tx, &ctx, *base, agent.as_ref())?
            }
            BoardOp::FeedbackList {
                open_only,
                after,
                through,
                limit,
            } => collection_reads::feedback_page(&tx, *open_only, *after, *through, *limit)?,
            BoardOp::Overview {
                scope,
                after,
                through,
                limit,
                ..
            } => collection_reads::overview(&tx, &ctx, scope, *after, *through, *limit)?,
            BoardOp::Attention {
                scope,
                after,
                through,
                limit,
            } => collection_reads::attention(&tx, &ctx, scope, *after, *through, *limit)?,
            BoardOp::Feed {
                plan,
                scope,
                after,
                through,
                limit,
            } => collection_reads::feed(&tx, *plan, scope, *after, *through, *limit)?,
            BoardOp::History {
                plan,
                after,
                through,
                limit,
            } => collection_reads::history(&tx, *plan, *after, *through, *limit)?,
            BoardOp::Entries {
                plan,
                kind,
                harness,
                user,
                host,
                task,
                references,
                after,
                before,
                through,
                limit,
            } => collection_reads::entries_page(
                &tx,
                *plan,
                *kind,
                harness.as_ref(),
                user.as_deref(),
                host.as_deref(),
                *task,
                *references,
                *after,
                *before,
                *through,
                *limit,
            )?,
            BoardOp::Tasks {
                plan,
                after,
                ceiling,
                through,
                limit,
            } => collection_nested::tasks_page(&tx, *plan, *after, *ceiling, *through, *limit)?,
            BoardOp::Claims {
                plan,
                own_stale,
                scope,
                after,
                through,
                limit,
            } => collection_nested::claims_page(
                &tx, &ctx, *plan, *own_stale, scope, *after, *through, *limit,
            )?,
            BoardOp::Repositories { plan } => board_reads::repositories(&tx, *plan)?,
            BoardOp::Projects => {
                return Err(invalid(
                    "invalid_options",
                    "projects require the serving host",
                ));
            }
            _ => {
                return Err(invalid(
                    "invalid_options",
                    "operation requires writable board storage",
                ));
            }
        };
        reply.backend = format!("local:{}", self.path.display());
        reply.snapshot_seq = Some(max_seq(&tx)?);
        tx.commit().map_err(sql_error)?;
        Ok(reply)
    }
}
