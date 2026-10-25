//! Local Git verification of progress evidence before acquiring the writer.
use super::{BoardHost, board_writer::check_deadline};
use crate::{
    board_ids::BoardRef,
    board_protocol::{BoardOp, BoardRequest},
};
use anyhow::Result;
use std::time::Duration;
use tp_core::query_deadline::QueryDeadline;

impl BoardHost {
    pub(super) fn verify_progress_head(
        &self,
        request: &BoardRequest,
        deadline: &QueryDeadline,
    ) -> Result<Option<BoardRequest>> {
        let BoardOp::Post {
            target: BoardRef::Task(task),
            progress: Some(progress),
            ..
        } = &request.op
        else {
            return Ok(None);
        };
        let Some(head) = progress.head else {
            return Ok(None);
        };
        if request.trusted_head.is_some() {
            return Ok(None);
        }
        let Ok(targets) = self.repositories(&request.actor, Some(task.plan), deadline) else {
            return Ok(None);
        };
        let spec = format!("{head}^{{commit}}");
        for target in targets {
            check_deadline(deadline)?;
            if let Ok(output) = tp_core::git_runner::run_bounded(
                &target.registration.common_dir,
                &["rev-parse", "--verify", "--end-of-options", &spec],
                deadline.cap(Duration::from_secs(2)),
            ) {
                if String::from_utf8_lossy(&output).trim() == head.as_str() {
                    let mut verified = request.clone();
                    verified.trusted_head = Some(head);
                    return Ok(Some(verified));
                }
            }
        }
        Ok(None)
    }
}
