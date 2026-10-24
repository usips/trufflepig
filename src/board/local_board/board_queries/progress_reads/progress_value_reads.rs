use crate::board::board_protocol::BoardErrorCode;
use rusqlite::{Connection, OptionalExtension, params};

use super::super::super::{BoardError, invalid, row_number, sql_error};
use crate::board::board_ids::EntryId;
use crate::board::board_protocol::{
    GateStatus, ProgressFact, ProgressGateFact, ProgressHead, ProgressStep, ProgressTests,
};
use crate::identity::GitOid;

pub(super) fn latest_head(
    conn: &Connection,
    claim_id: i64,
    worker_id: Option<i64>,
) -> Result<Option<ProgressFact<ProgressHead>>, BoardError> {
    let row = conn
        .query_row(
            concat!(
                "SELECT head_oid,head_verified,entry_id,updated_at FROM progress_facts ",
                "WHERE claim_id=?1 AND worker_id IS ?2 AND head_oid IS NOT NULL ",
                "ORDER BY entry_id DESC LIMIT 1"
            ),
            params![claim_id, worker_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row_number(row, 2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()
        .map_err(sql_error)?;
    row.map(|(oid, verified, entry, updated_at)| {
        let oid = GitOid::parse(&oid).map_err(|error| {
            invalid(
                BoardErrorCode::BoardUnavailable,
                format!("invalid stored progress OID: {error}"),
            )
        })?;
        let verified = match verified {
            Some(0) => false,
            Some(1) => true,
            _ => {
                return Err(invalid(
                    BoardErrorCode::BoardUnavailable,
                    "invalid stored progress head verification",
                ));
            }
        };
        Ok(ProgressFact {
            value: ProgressHead { oid, verified },
            entry: EntryId::new(entry).map_err(BoardError::from)?,
            updated_at,
        })
    })
    .transpose()
}

pub(super) fn latest_step(
    conn: &Connection,
    claim_id: i64,
    worker_id: Option<i64>,
) -> Result<Option<ProgressFact<ProgressStep>>, BoardError> {
    let row = conn
        .query_row(
            concat!(
                "SELECT step_done,step_total,entry_id,updated_at FROM progress_facts ",
                "WHERE claim_id=?1 AND worker_id IS ?2 AND step_done IS NOT NULL ",
                "ORDER BY entry_id DESC LIMIT 1"
            ),
            params![claim_id, worker_id],
            |row| {
                Ok((
                    row_number(row, 0)?,
                    row_number(row, 1)?,
                    row_number(row, 2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()
        .map_err(sql_error)?;
    row.map(|(current, total, entry, updated_at)| {
        if total == 0 || current > total {
            return Err(invalid(
                BoardErrorCode::BoardUnavailable,
                "invalid stored progress step",
            ));
        }
        Ok(ProgressFact {
            value: ProgressStep { current, total },
            entry: EntryId::new(entry).map_err(BoardError::from)?,
            updated_at,
        })
    })
    .transpose()
}

pub(super) fn latest_tests(
    conn: &Connection,
    claim_id: i64,
    worker_id: Option<i64>,
) -> Result<Option<ProgressFact<ProgressTests>>, BoardError> {
    let row = conn
        .query_row(
            concat!(
                "SELECT tests_passed,tests_failed,entry_id,updated_at FROM progress_facts ",
                "WHERE claim_id=?1 AND worker_id IS ?2 AND tests_passed IS NOT NULL ",
                "ORDER BY entry_id DESC LIMIT 1"
            ),
            params![claim_id, worker_id],
            |row| {
                Ok((
                    row_number(row, 0)?,
                    row_number(row, 1)?,
                    row_number(row, 2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()
        .map_err(sql_error)?;
    row.map(|(passed, failed, entry, updated_at)| {
        Ok(ProgressFact {
            value: ProgressTests { passed, failed },
            entry: EntryId::new(entry).map_err(BoardError::from)?,
            updated_at,
        })
    })
    .transpose()
}

pub(super) fn latest_eta(
    conn: &Connection,
    claim_id: i64,
    worker_id: Option<i64>,
) -> Result<Option<ProgressFact<i64>>, BoardError> {
    let row = conn
        .query_row(
            concat!(
                "SELECT eta_at,entry_id,updated_at FROM progress_facts ",
                "WHERE claim_id=?1 AND worker_id IS ?2 AND eta_at IS NOT NULL ",
                "ORDER BY entry_id DESC LIMIT 1"
            ),
            params![claim_id, worker_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row_number(row, 1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()
        .map_err(sql_error)?;
    row.map(|(value, entry, updated_at)| {
        Ok(ProgressFact {
            value,
            entry: EntryId::new(entry).map_err(BoardError::from)?,
            updated_at,
        })
    })
    .transpose()
}

pub(super) fn latest_gates(
    conn: &Connection,
    claim_id: i64,
    worker_id: Option<i64>,
) -> Result<Vec<ProgressGateFact>, BoardError> {
    let mut statement = conn
        .prepare(concat!(
            "SELECT g.name,g.result,g.entry_id,p.updated_at FROM progress_gates g ",
            "JOIN progress_facts p ON p.entry_id=g.entry_id ",
            "WHERE p.claim_id=?1 AND p.worker_id IS ?2 AND NOT EXISTS (",
            "SELECT 1 FROM progress_gates newer JOIN progress_facts newer_progress ",
            "ON newer_progress.entry_id=newer.entry_id WHERE newer.name=g.name ",
            "AND newer_progress.claim_id=p.claim_id ",
            "AND newer_progress.worker_id IS p.worker_id ",
            "AND newer.entry_id>g.entry_id) ",
            "ORDER BY g.name"
        ))
        .map_err(sql_error)?;
    let mut rows = statement
        .query(params![claim_id, worker_id])
        .map_err(sql_error)?;
    let mut gates = Vec::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        let name = row.get::<_, String>(0).map_err(sql_error)?;
        let result = row.get::<_, String>(1).map_err(sql_error)?;
        let status = match result.as_str() {
            "pass" => GateStatus::Pass,
            "fail" => GateStatus::Fail,
            "running" => GateStatus::Running,
            _ => {
                return Err(invalid(
                    BoardErrorCode::BoardUnavailable,
                    "invalid stored progress gate result",
                ));
            }
        };
        gates.push(ProgressGateFact {
            name,
            status,
            entry: EntryId::new(row_number(row, 2).map_err(sql_error)?)
                .map_err(BoardError::from)?,
            updated_at: row.get(3).map_err(sql_error)?,
        });
    }
    Ok(gates)
}
