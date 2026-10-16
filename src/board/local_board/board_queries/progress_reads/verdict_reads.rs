use std::convert::TryFrom;

use rusqlite::{Connection, OptionalExtension, params};

use super::super::super::{BoardError, invalid, sql_error, sql_number, sqlite_u64};
use crate::board::board_protocol::{ReviewVerdict, WorkerAddress};

pub(in crate::board::local_board) fn verdict_for_entry(
    conn: &Connection,
    entry: crate::board::board_ids::EntryId,
) -> Result<Option<ReviewVerdict>, BoardError> {
    let row = conn
        .query_row(
            "SELECT verdict,count FROM review_verdicts WHERE entry_id=?1",
            [sql_number(entry.get())],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?)),
        )
        .optional()
        .map_err(sql_error)?;
    row.map(|(kind, count)| decode_verdict(&kind, count))
        .transpose()
}

pub(in crate::board::local_board) fn latest_worker_verdict(
    conn: &Connection,
    worker: &WorkerAddress,
) -> Result<Option<ReviewVerdict>, BoardError> {
    let worker_id = conn
        .query_row(
            "SELECT id FROM claim_workers WHERE plan_id=?1 AND task_ordinal=?2 AND handle=?3",
            params![
                sql_number(worker.task.plan.get()),
                sql_number(worker.task.ordinal),
                worker.handle.as_str(),
            ],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(sql_error)?;
    let Some(worker_id) = worker_id else {
        return Ok(None);
    };
    let row = conn
        .query_row(
            concat!(
                "SELECT v.verdict,v.count FROM review_verdicts v ",
                "JOIN entries e ON e.id=v.entry_id WHERE e.worker_id=?1 AND e.kind='review' ",
                "ORDER BY e.seq DESC,e.id DESC LIMIT 1"
            ),
            [worker_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?)),
        )
        .optional()
        .map_err(sql_error)?;
    row.map(|(kind, count)| decode_verdict(&kind, count))
        .transpose()
}

fn decode_verdict(kind: &str, count: Option<i64>) -> Result<ReviewVerdict, BoardError> {
    match (kind, count) {
        ("clean", None) => Ok(ReviewVerdict::Clean),
        ("findings", Some(count)) => Ok(ReviewVerdict::Findings(verdict_count(count)?)),
        ("blocking", Some(count)) => Ok(ReviewVerdict::Blocking(verdict_count(count)?)),
        _ => Err(invalid(
            "board_unavailable",
            "invalid stored review verdict",
        )),
    }
}

fn verdict_count(count: i64) -> Result<u32, BoardError> {
    let count = sqlite_u64(count)?;
    u32::try_from(count).map_err(|_| {
        invalid(
            "board_unavailable",
            "stored review verdict count exceeds u32",
        )
    })
}
