//! Manual commit links resolve one oid in the plan's registered repositories.
use super::BoardHost;
use crate::{
    board::{
        board_ids::TaskId,
        board_protocol::{BoardError, BoardErrorCode, BoardOp, BoardRequest},
        local_board::LocalBoard,
    },
    daemon::deadline::QueryDeadline,
    identity::GitOid,
};
use anyhow::Result;
use std::time::Duration;

impl BoardHost {
    /// Fills a manual link's resolution from Git before the write dispatch.
    pub(super) fn resolve_link_commit(
        &self,
        request: &BoardRequest,
        oid: GitOid,
        task: TaskId,
        deadline: QueryDeadline,
    ) -> Result<BoardRequest> {
        let targets = self.repositories(&request.actor, Some(task.plan), deadline)?;
        let config = self.config()?;
        let mut reader =
            LocalBoard::open_read_with_timeout(&config, deadline.cap(Duration::from_secs(5)))?;
        reader.require_commit_link_authority(&request.actor, task.plan)?;
        if targets.is_empty() {
            return Err(BoardError::new(
                BoardErrorCode::InvalidReference,
                format!(
                    "{} has no registered repository on {}; run any {} board write from the checkout first",
                    task.plan, request.actor.host, task.plan
                ),
            )
            .into());
        }
        let mut resolved = None;
        for target in &targets {
            let spec = format!("{oid}^{{commit}}");
            let Ok(peeled) = crate::history::git::run_bounded(
                &target.registration.common_dir,
                &["rev-parse", "--verify", "--end-of-options", spec.as_str()],
                deadline.cap(Duration::from_secs(5)),
            ) else {
                continue;
            };
            if String::from_utf8_lossy(&peeled).trim() != oid.as_str() {
                return Err(BoardError::new(
                    BoardErrorCode::InvalidReference,
                    format!("{oid} names a tag; pass the commit id"),
                )
                .into());
            }
            resolved = Some(crate::board::commit_ingest::read_commit(
                &target.registration,
                oid,
                deadline.remaining(),
            )?);
            break;
        }
        let Some(commit) = resolved else {
            return Err(BoardError::new(
                BoardErrorCode::InvalidReference,
                format!(
                    "unknown commit {oid} in the registered repositories of {}",
                    task.plan
                ),
            )
            .into());
        };
        let mut request = request.clone();
        request.op = BoardOp::LinkCommit {
            oid,
            task,
            resolution: Some(Box::new(commit)),
        };
        Ok(request)
    }
}
