//! Query-only plan counts for repository sets supplied by the serving host.
use super::super::{BoardError, LocalBoard, invalid, max_seq, sql_error};
use crate::board::board_ids::{EventSeq, RepoKey};
use crate::board::board_protocol::BoardErrorCode;
use crate::daemon::deadline::QueryDeadline;
use std::collections::BTreeSet;

impl LocalBoard {
    pub(crate) fn repo_set_plan_counts(
        &mut self,
        sets: &[&BTreeSet<RepoKey>],
        deadline: QueryDeadline,
    ) -> Result<(EventSeq, Vec<usize>), BoardError> {
        let conn = self.reader.as_mut().unwrap_or(&mut self.conn);
        conn.progress_handler(1000, Some(move || deadline.expired()))
            .map_err(sql_error)?;
        let result =
            (|| {
                let tx = conn
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)
                    .map_err(sql_error)?;
                let snapshot = max_seq(&tx)?;
                let mut counts = Vec::with_capacity(sets.len());
                {
                    let mut statement = tx
                        .prepare(concat!(
                            "SELECT count(DISTINCT plan_id) FROM plan_repos ",
                            "WHERE repo_key IN(SELECT value FROM json_each(?1))"
                        ))
                        .map_err(sql_error)?;
                    for keys in sets {
                        if deadline.expired() {
                            return Err(count_timeout());
                        }
                        let encoded = serde_json::to_string(keys)
                            .map_err(|error| invalid("invalid_options", error.to_string()))?;
                        let count: i64 = statement
                            .query_row([encoded], |row| row.get(0))
                            .map_err(sql_error)?;
                        counts.push(usize::try_from(count).map_err(|_| {
                            invalid("board_unavailable", "invalid project plan count")
                        })?);
                    }
                }
                tx.commit().map_err(sql_error)?;
                Ok((snapshot, counts))
            })();
        let cleared = conn
            .progress_handler(0, None::<fn() -> bool>)
            .map_err(sql_error);
        if deadline.expired() {
            return Err(count_timeout());
        }
        cleared?;
        result
    }
}

fn count_timeout() -> BoardError {
    BoardError::new(BoardErrorCode::DaemonBusy, "project read timed out")
}
