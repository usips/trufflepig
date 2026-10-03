//! Sequence-gated inbox waiting and bounded waiter admission.

use super::{BoardHost, board_writer::recover_lock};
use crate::{
    board::{
        board_ids::EventSeq,
        board_protocol::{BoardOp, BoardReply, BoardRequest, BoardResult, InboxWait},
    },
    daemon::deadline::QueryDeadline,
};
use anyhow::Result;
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

pub(super) const MAX_WAITERS: usize = 6;
const MAX_INBOX_WAIT: Duration = Duration::from_secs(15);

impl BoardHost {
    pub(super) fn wait_inbox(
        &self,
        request: &BoardRequest,
        deadline: QueryDeadline,
    ) -> Result<BoardReply> {
        let mut reply = match self.handle_by(request, deadline) {
            Ok(reply) => reply,
            Err(error) if waiter_transient(&error) => {
                let BoardOp::Inbox { after, .. } = &request.op else {
                    return Err(error);
                };
                let cursor = after.unwrap_or(EventSeq::new(0));
                let config = self.config()?;
                return Ok(BoardReply::new(
                    format!("local:{}", config.db_path.display()),
                    BoardResult::Inbox(crate::board::board_protocol::InboxReply {
                        actor: request.actor.clone(),
                        cursor,
                        events: Vec::new(),
                        open: Vec::new(),
                        latest: cursor,
                        scanned_through: cursor,
                        advancing: false,
                        repo_key: None,
                        all: false,
                        open_omitted: 0,
                        query_truncated: false,
                        wait: InboxWait::Timeout,
                    }),
                ));
            }
            Err(error) => return Err(error),
        };
        if inbox_has_events(&reply) {
            set_wait(&mut reply, InboxWait::Ready);
            return Ok(reply);
        }
        let Some(_permit) = BoardInboxWaiterPermit::acquire(&self.inner.waiters) else {
            set_wait(&mut reply, InboxWait::Busy);
            return Ok(reply);
        };
        let timeout = deadline
            .remaining()
            .saturating_sub(Duration::from_secs(2))
            .min(MAX_INBOX_WAIT);
        let expires = Instant::now() + timeout;
        let mut observed = recover_lock(&self.inner.sequence);
        loop {
            let known = match &reply.result {
                BoardResult::Inbox(inbox) => inbox.latest,
                _ => EventSeq::new(0),
            };
            let remaining = expires.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                set_wait(&mut reply, InboxWait::Timeout);
                return Ok(reply);
            }
            if *observed <= known {
                let (guard, _) = self
                    .inner
                    .changed
                    .wait_timeout(observed, remaining.min(Duration::from_secs(1)))
                    .unwrap_or_else(|poisoned| {
                        let (mut guard, timeout) = poisoned.into_inner();
                        *guard = EventSeq::default();
                        self.inner.sequence.clear_poison();
                        (guard, timeout)
                    });
                observed = guard;
            }
            drop(observed);
            let polling = deadline.capped(Duration::from_millis(100));
            let sequence = match self.max_seq_by(polling) {
                Ok(sequence) => sequence,
                Err(error) if waiter_transient(&error) => {
                    set_wait(&mut reply, InboxWait::Timeout);
                    return Ok(reply);
                }
                Err(error) => return Err(error),
            };
            if sequence > known {
                reply = match self.handle_by(request, polling) {
                    Ok(reply) => reply,
                    Err(error) if waiter_transient(&error) => {
                        set_wait(&mut reply, InboxWait::Timeout);
                        return Ok(reply);
                    }
                    Err(error) => return Err(error),
                };
                if inbox_has_events(&reply) {
                    set_wait(&mut reply, InboxWait::Ready);
                    return Ok(reply);
                }
            }
            observed = recover_lock(&self.inner.sequence);
        }
    }
}

pub(super) struct BoardInboxWaiterPermit<'a>(&'a AtomicUsize);
impl<'a> BoardInboxWaiterPermit<'a> {
    pub(super) fn acquire(waiters: &'a AtomicUsize) -> Option<Self> {
        waiters
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_WAITERS).then_some(count + 1)
            })
            .ok()?;
        Some(Self(waiters))
    }
}
impl Drop for BoardInboxWaiterPermit<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
pub(super) fn inbox_has_events(reply: &BoardReply) -> bool {
    matches!(&reply.result, BoardResult::Inbox(inbox) if !inbox.events.is_empty())
}
fn set_wait(reply: &mut BoardReply, wait: InboxWait) {
    if let BoardResult::Inbox(inbox) = &mut reply.result {
        inbox.wait = wait;
    }
}

pub(super) fn waiter_transient(error: &anyhow::Error) -> bool {
    crate::daemon::deadline::is_timed_out(error)
        || crate::board::board_protocol::BoardErrorCode::from_error(error)
            == Some(crate::board::board_protocol::BoardErrorCode::DatabaseLocked)
}
