use crate::board::board_protocol::BoardErrorCode;
use std::convert::TryFrom;

use super::super::super::{BoardError, WriteContext, invalid};
use crate::board::board_protocol::ProgressInput;
use crate::identity::GitOid;

pub(super) struct StoredProgressFields {
    pub head_oid: Option<String>,
    pub head_verified: Option<i64>,
    pub step: Option<(i64, i64)>,
    pub tests: Option<(i64, i64)>,
    pub eta_at: Option<i64>,
}

pub(super) fn validate_progress(
    input: &ProgressInput,
    trusted_head: Option<&GitOid>,
    ctx: &WriteContext,
) -> Result<StoredProgressFields, BoardError> {
    let has_facts = input.head.is_some()
        || input.step.is_some()
        || input.tests.is_some()
        || input.eta_secs.is_some()
        || !input.gates.is_empty();
    if !has_facts {
        return Err(invalid(
            BoardErrorCode::InvalidOptions,
            "structured progress must supply at least one field",
        ));
    }
    if input.gates.len() > 64 {
        return Err(invalid(
            BoardErrorCode::InvalidOptions,
            "progress may update at most 64 gates at once",
        ));
    }

    let (head_oid, head_verified) = match (input.head.as_ref(), trusted_head) {
        (Some(head), Some(verified)) if head == verified => (Some(head.to_string()), Some(1_i64)),
        (Some(_), Some(_)) => {
            return Err(invalid(
                BoardErrorCode::BoardUnavailable,
                "host progress head verification does not match the submitted object ID",
            ));
        }
        (Some(head), None) => (Some(head.to_string()), Some(0_i64)),
        (None, Some(_)) => {
            return Err(invalid(
                BoardErrorCode::BoardUnavailable,
                "host supplied progress head verification without a submitted head",
            ));
        }
        (None, None) => (None, None),
    };

    let step = input
        .step
        .as_ref()
        .map(|step| -> Result<(i64, i64), BoardError> {
            if step.total == 0 || step.current > step.total {
                return Err(invalid(
                    BoardErrorCode::InvalidOptions,
                    "progress step requires 0 <= current <= total and total > 0",
                ));
            }
            Ok((
                checked_sql_count(step.current, "step current")?,
                checked_sql_count(step.total, "step total")?,
            ))
        })
        .transpose()?;
    let tests = input
        .tests
        .as_ref()
        .map(|tests| -> Result<(i64, i64), BoardError> {
            Ok((
                checked_sql_count(tests.passed, "tests passed")?,
                checked_sql_count(tests.failed, "tests failed")?,
            ))
        })
        .transpose()?;
    let eta_at = input
        .eta_secs
        .map(|eta_secs| {
            if eta_secs == 0 || eta_secs > 31_536_000 {
                return Err(invalid(
                    BoardErrorCode::InvalidOptions,
                    "progress ETA must be between 1 second and 365 days",
                ));
            }
            let eta_secs = i64::try_from(eta_secs).map_err(|_| {
                invalid(
                    BoardErrorCode::InvalidOptions,
                    "progress ETA exceeds SQLite integer range",
                )
            })?;
            ctx.now.checked_add(eta_secs).ok_or_else(|| {
                invalid(
                    BoardErrorCode::InvalidOptions,
                    "progress ETA timestamp overflow",
                )
            })
        })
        .transpose()?;

    for (index, gate) in input.gates.iter().enumerate() {
        let name = gate.name.as_str();
        if name.is_empty()
            || name.len() > 64
            || name.trim().is_empty()
            || name.chars().any(char::is_control)
            || name.contains('=')
        {
            return Err(invalid(
                BoardErrorCode::InvalidOptions,
                "progress gate name must be 1..64 UTF-8 bytes without whitespace-only, control, or '=' characters",
            ));
        }
        if input.gates[..index]
            .iter()
            .any(|previous| previous.name == gate.name)
        {
            return Err(invalid(
                BoardErrorCode::InvalidOptions,
                format!("progress gate `{name}` is supplied more than once"),
            ));
        }
    }

    Ok(StoredProgressFields {
        head_oid,
        head_verified,
        step,
        tests,
        eta_at,
    })
}

fn checked_sql_count(value: u64, field: &str) -> Result<i64, BoardError> {
    i64::try_from(value).map_err(|_| {
        invalid(
            BoardErrorCode::InvalidOptions,
            format!("{field} exceeds SQLite integer range"),
        )
    })
}
