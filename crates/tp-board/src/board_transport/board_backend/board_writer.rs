//! Deadline-bound writer access and imported-feedback dispatch.

use super::{BoardBackend, BoardHost};
use crate::{
    board_ids::EventSeq,
    board_protocol::{
        BoardError, BoardOp, BoardReply, BoardRequest, BoardResult, ByeOutcome, ByeReply,
    },
    local_board::LocalBoard,
};
use anyhow::{Result, bail};
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::{
    sync::{Mutex, MutexGuard, TryLockError},
    time::Duration,
};
use tp_core::query_deadline::QueryDeadline;

impl BoardHost {
    /// Opens the writer, creating and migrating the database when needed. The
    /// router calls this after binding but before accepting connections, so
    /// waiting clients queue while status never races migration; request paths
    /// call it lazily, and starters ignore failure to keep serving.
    pub fn ensure_writer(&self, deadline: QueryDeadline) -> Result<()> {
        check_deadline(&deadline)?;
        let config = self.config()?;
        let mut backend = lock_before(&self.inner.backend, &deadline, "writer")?;
        if backend.is_none() {
            *backend = Some(LocalBoard::open_with_timeout(
                &config,
                deadline.cap(Duration::from_secs(5)),
            )?);
        }
        Ok(())
    }

    pub(super) fn handle_by(
        &self,
        request: &BoardRequest,
        deadline: &QueryDeadline,
    ) -> Result<BoardReply> {
        self.handle_by_mode(request, deadline, false)
    }

    pub(super) fn handle_imported_by(
        &self,
        request: &BoardRequest,
        deadline: &QueryDeadline,
    ) -> Result<BoardReply> {
        ensure_feedback_import(request)?;
        self.handle_by_mode(request, deadline, true)
    }

    fn handle_by_mode(
        &self,
        request: &BoardRequest,
        deadline: &QueryDeadline,
        imported: bool,
    ) -> Result<BoardReply> {
        check_deadline(deadline)?;
        request.validate()?;
        let verified = self.verify_progress_head(request, deadline)?;
        let request = verified.as_ref().unwrap_or(request);
        if matches!(request.op, BoardOp::Projects) {
            let config = self.config()?;
            let projects = self.projects(&request.actor.host, deadline)?;
            return crate::board_projects::project_reply(&config, projects, deadline)
                .map_err(Into::into);
        }
        if !imported
            && let BoardOp::LinkCommit {
                oid,
                task,
                resolution: None,
            } = &request.op
        {
            let request = self.resolve_link_commit(request, *oid, *task, deadline)?;
            return self.handle_by_mode(&request, deadline, false);
        }
        #[cfg(test)]
        if matches!(request.op, BoardOp::Inbox { .. }) {
            self.inner.inbox_queries.fetch_add(1, Ordering::Relaxed);
        }
        let config = self.config()?;
        if request.op.is_read_only() {
            let mut reader = match LocalBoard::open_read_with_timeout(
                &config,
                deadline.cap(Duration::from_secs(5)),
            ) {
                Ok(reader) => reader,
                Err(error)
                    if !config.db_path.exists()
                        || LocalBoard::needs_writable_initialization(&error) =>
                {
                    if matches!(request.op, BoardOp::Pending { .. }) {
                        return Err(error.into());
                    }
                    if LocalBoard::needs_writable_initialization(&error)
                        && !board_storage_owner_writable(&config.db_path)
                    {
                        return Err(error.into());
                    }
                    self.ensure_writer(deadline.clone())?;
                    LocalBoard::open_read_with_timeout(
                        &config,
                        deadline.cap(Duration::from_secs(5)),
                    )?
                }
                Err(error) => return Err(error.into()),
            };
            check_deadline(deadline)?;
            let reply = reader.handle(request)?;
            reply.validate()?;
            return Ok(reply);
        }
        if matches!(request.op, BoardOp::Bye)
            && let Some(reply) = settled_bye(&config, request, deadline)
        {
            return Ok(reply);
        }
        let (reply, seq) = {
            let mut backend = lock_before(&self.inner.backend, deadline, "writer")?;
            if backend.is_none() {
                *backend = Some(LocalBoard::open_with_timeout(
                    &config,
                    deadline.cap(Duration::from_secs(5)),
                )?);
            }
            let backend = backend.as_mut().expect("backend just initialized");
            check_deadline(deadline)?;
            backend.set_busy_timeout(deadline.cap(Duration::from_secs(5)))?;
            backend.set_claim_ttl(Duration::from_secs(config.claim_ttl_minutes * 60))?;
            let reply = if imported {
                backend.import_feedback(request)?
            } else {
                backend.handle(request)?
            };
            reply.validate()?;
            let seq = if deadline.expired() {
                None
            } else {
                let _ = backend.set_busy_timeout(deadline.cap(Duration::from_millis(100)));
                backend.max_seq().ok()
            };
            (reply, seq)
        };
        // Housekeeping after a committed mutation cannot turn its receipt into
        // a retryable failure. Polling waiters also see writes without notification.
        if let Some(seq) = seq {
            let mut sequence = recover_lock(&self.inner.sequence);
            if seq > *sequence {
                *sequence = seq;
                self.inner.changed.notify_all();
            }
        }
        Ok(reply)
    }
}

/// Answers a bye with nothing to end from a reader, so hook byes from sessions
/// that never said hello neither take the writer nor create storage. Any read
/// failure defers to the writer path.
fn settled_bye(
    config: &crate::BoardConfig,
    request: &BoardRequest,
    deadline: &QueryDeadline,
) -> Option<BoardReply> {
    if !config.db_path.exists() {
        return Some(BoardReply::new(
            format!("local:{}", config.db_path.display()),
            BoardResult::Bye(ByeReply {
                outcome: ByeOutcome::NoSession,
                entry: None,
                seq: None,
            }),
        ));
    }
    LocalBoard::open_read_with_timeout(config, deadline.cap(Duration::from_secs(5)))
        .ok()?
        .settled_bye(&request.actor)
        .ok()
        .flatten()
}

pub(super) struct BoardHostBackendAccess<'a>(pub(super) &'a BoardHost, pub(super) QueryDeadline);
fn board_storage_owner_writable(database: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    database
        .metadata()
        .is_ok_and(|metadata| metadata.permissions().mode() & 0o200 != 0)
        && database.parent().is_some_and(|parent| {
            parent
                .metadata()
                .is_ok_and(|metadata| metadata.permissions().mode() & 0o200 != 0)
        })
}

impl BoardBackend for BoardHostBackendAccess<'_> {
    fn handle(&mut self, request: &BoardRequest) -> std::result::Result<BoardReply, BoardError> {
        self.0.handle_by(request, &self.1).map_err(Into::into)
    }
    fn import_feedback(
        &mut self,
        request: &BoardRequest,
    ) -> std::result::Result<BoardReply, BoardError> {
        self.0
            .handle_imported_by(request, &self.1)
            .map_err(Into::into)
    }
    fn max_seq(&self) -> std::result::Result<EventSeq, BoardError> {
        self.0.max_seq_by(&self.1).map_err(BoardError::from)
    }
    fn linked_commit_oids(
        &self,
        repo_key: &crate::board_ids::RepoKey,
        oids: &[tp_core::identity::GitOid],
    ) -> std::result::Result<std::collections::BTreeSet<tp_core::identity::GitOid>, BoardError>
    {
        self.0
            .linked_commit_oids_by(repo_key, oids, &self.1)
            .map_err(BoardError::from)
    }
}

pub(crate) fn ensure_feedback_import(
    request: &BoardRequest,
) -> std::result::Result<(), BoardError> {
    if !matches!(
        &request.op,
        BoardOp::Feedback {
            import_key: Some(_),
            ..
        }
    ) {
        return Err(BoardError::new(
            crate::board_protocol::BoardErrorCode::InvalidOptions,
            "import requires feedback with its permanent identity",
        ));
    }
    Ok(())
}

pub(super) fn check_deadline(deadline: &QueryDeadline) -> Result<()> {
    if deadline.expired() {
        bail!(coded!(TimedOut, "board query deadline expired"));
    }
    Ok(())
}

pub(super) fn recover_lock<T: Default>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        let mut guard = poisoned.into_inner();
        *guard = T::default();
        mutex.clear_poison();
        guard
    })
}

pub(super) fn lock_before<'a, T: Default>(
    mutex: &'a Mutex<T>,
    deadline: &QueryDeadline,
    _label: &str,
) -> Result<MutexGuard<'a, T>> {
    loop {
        check_deadline(deadline)?;
        match mutex.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::Poisoned(poisoned)) => {
                let mut guard = poisoned.into_inner();
                *guard = T::default();
                mutex.clear_poison();
                return Ok(guard);
            }
            Err(TryLockError::WouldBlock) => {
                std::thread::sleep(deadline.cap(Duration::from_millis(5)))
            }
        }
    }
}
