//! Committed read snapshots and query-only dispatch.

use super::*;

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
        let events = board_feed::read_events(&tx, after, latest, plan, limit)?;
        tx.commit().map_err(sql_error)?;
        Ok((latest, events))
    }

    pub(super) fn dispatch_read(
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
                repo_key,
                all,
            } => board_feed::inbox(&tx, &ctx, *after, *limit, repo_key.as_ref(), *all)?,
            BoardOp::Show { target } => board_reads::show(&tx, &ctx, target.as_ref())?,
            BoardOp::Review { base, agent } => {
                board_reads::review(&tx, &ctx, *base, agent.as_ref())?
            }
            BoardOp::FeedbackList { open_only } => {
                feedback_entries::list_feedback(&tx, *open_only)?
            }
            BoardOp::Repositories { plan } => board_reads::repositories(&tx, *plan)?,
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
