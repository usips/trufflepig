use super::IndexCheckpoint;
use crate::daemon::deadline::QueryDeadline;
use crate::store::{Coverage, index_deadline, scan};
use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

pub(super) fn stage(
    checkpoint: &mut IndexCheckpoint,
    staged: &mut Connection,
    previous: &Connection,
    root: &Path,
    cache: &Path,
    deadline: Option<&QueryDeadline>,
) -> Result<(Coverage, String)> {
    let result = scan::stage_checkpointed(staged, previous, root, cache, checkpoint, deadline);
    match result {
        Ok(result) => Ok(result),
        Err(error) => {
            // Files saved before the failure are valid facts; keep them for the next attempt.
            let _ = checkpoint.commit();
            match deadline.filter(|deadline| deadline.expired()) {
                Some(deadline)
                    if deadline.time_expired()
                        && !deadline.cancelled()
                        && index_deadline::is_deadline_interruption(&error) =>
                {
                    let _ = checkpoint.note_budget_expired(deadline);
                }
                _ => {
                    let _ = checkpoint.record_progress();
                }
            }
            Err(error)
        }
    }
}
