use super::IndexCheckpoint;
use crate::store::{Coverage, index_deadline, scan};
use anyhow::Result;
use rusqlite::Connection;
use tp_core::query_deadline::QueryDeadline;

pub(super) fn stage(
    checkpoint: &mut IndexCheckpoint,
    staged: &mut Connection,
    previous: &Connection,
    observation: scan::TreeObservation,
    inputs: &scan::StageInputs<'_>,
    deadline: Option<&QueryDeadline>,
) -> Result<(Coverage, String, scan::StatRefresh)> {
    let result = scan::stage(
        staged,
        previous,
        observation,
        inputs,
        deadline,
        Some(checkpoint),
    );
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
