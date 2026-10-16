use crate::daemon::deadline::{QueryDeadline, TIMED_OUT, is_timed_out};
use anyhow::{Result, bail};
use rusqlite::{Connection, Error as SqliteError, ErrorCode};
use std::time::Duration;

pub(super) const SQLITE_PROGRESS_STEPS: i32 = 4_096;
pub(super) const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(2);
pub(super) const SQLITE_DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(30);
const INLINE_MAINTENANCE_MAX: Duration = Duration::from_secs(2);
const INLINE_MAINTENANCE_RESERVE: Duration = Duration::from_millis(250);

pub(super) fn inline_maintenance_eligibility() -> Duration {
    INLINE_MAINTENANCE_MAX.saturating_sub(INLINE_MAINTENANCE_RESERVE)
}

pub fn inline_maintenance_deadline(deadline: &QueryDeadline) -> QueryDeadline {
    let remaining = deadline
        .remaining()
        .saturating_sub(INLINE_MAINTENANCE_RESERVE)
        .min(INLINE_MAINTENANCE_MAX);
    deadline.capped(remaining)
}

pub(super) fn check(deadline: &QueryDeadline) -> Result<()> {
    if deadline.expired() {
        bail!("{TIMED_OUT}: query deadline expired");
    }
    Ok(())
}

pub(super) fn install(conn: &Connection, deadline: &QueryDeadline) -> Result<()> {
    check(deadline)?;
    attach(conn, deadline)?;
    check(deadline)
}

/// Installs the deadline's busy timeout and progress handler without checking expiry.
pub(super) fn attach(conn: &Connection, deadline: &QueryDeadline) -> Result<()> {
    conn.busy_timeout(deadline.cap(SQLITE_BUSY_TIMEOUT))?;
    let progress_deadline = deadline.clone();
    conn.progress_handler(
        SQLITE_PROGRESS_STEPS,
        Some(move || progress_deadline.expired()),
    )?;
    Ok(())
}

pub(super) fn clear(conn: &Connection) -> Result<()> {
    conn.progress_handler(0, None::<fn() -> bool>)?;
    conn.busy_timeout(SQLITE_DEFAULT_BUSY_TIMEOUT)?;
    Ok(())
}

pub(super) fn classify(deadline: &QueryDeadline, error: anyhow::Error) -> anyhow::Error {
    let error = deadline.classify(error);
    if deadline.expired() && !is_timed_out(&error) {
        error.context(format!("{TIMED_OUT}: query deadline expired"))
    } else {
        error
    }
}

pub(super) fn is_deadline_interruption(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.to_string().starts_with("timed_out:")
            || cause.downcast_ref::<SqliteError>().is_some_and(|error| {
                matches!(
                    error,
                    SqliteError::SqliteFailure(failure, _)
                        if failure.code == ErrorCode::OperationInterrupted
                )
            })
    })
}
