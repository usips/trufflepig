//! Router identity and schema probing for startup and ingest relay.
use super::super::deadline_error;
use crate::board::{
    SCHEMA_VERSION,
    board_protocol::{BOARD_API, BoardError, BoardErrorCode},
};
use crate::daemon::deadline::QueryDeadline;
use std::{
    path::Path,
    time::{Duration, Instant},
};

type RouterExchange<'a> =
    dyn FnMut(&[String], QueryDeadline) -> Result<Option<String>, BoardError> + 'a;

/// How long startup waits for a concurrently-starting router to migrate the
/// database, and how often it re-polls `system status` while waiting.
pub(super) const ROUTER_MIGRATION_WAIT: Duration = Duration::from_secs(30);
const ROUTER_MIGRATION_POLL: Duration = Duration::from_millis(500);

/// Startup opens the database only when a same-API router confirms its schema,
/// the file already holds the supported schema, or a brand-new database may
/// bootstrap. An answered router error is fatal; only a silent router (`None`)
/// permits the local schema check. A stale local file waits up to `wait` for a
/// concurrently-starting router to migrate it; a newer file refuses at once.
/// After the wait, an answering router is still migrating while a silent one
/// was never started, and each refusal says so.
pub(super) fn startup_probe(
    database: &Path,
    deadline: QueryDeadline,
    wait: Duration,
    exchange: &mut RouterExchange<'_>,
) -> Result<(), BoardError> {
    let started = Instant::now();
    let mut answered = false;
    loop {
        // The startup wait outlives the request deadline; each poll stays bounded.
        let poll = if deadline.expired() {
            QueryDeadline::after(ROUTER_MIGRATION_POLL)
        } else {
            deadline
        };
        let status = probe_router(database, poll, exchange)?;
        answered |= status.is_some();
        if status.is_some_and(|status| router_confirms_schema(&status)) {
            return Ok(());
        }
        match local_schema_version(database)? {
            None => return Ok(()),
            Some(version) if version == SCHEMA_VERSION => return Ok(()),
            Some(version) if version < SCHEMA_VERSION => {
                if started.elapsed() >= wait {
                    if answered {
                        return Err(unavailable(
                            "router is migrating; check `systemctl --user status trufflepig-system`",
                        ));
                    }
                    return Err(unavailable(format!(
                        "board database schema version {version} awaits migration to {SCHEMA_VERSION}; start trufflepig system ensure with the current binary"
                    )));
                }
                std::thread::sleep(
                    ROUTER_MIGRATION_POLL.min(wait.saturating_sub(started.elapsed())),
                );
            }
            Some(version) => {
                return Err(unavailable(format!(
                    "board database schema version {version} is newer than supported {SCHEMA_VERSION}"
                )));
            }
        }
    }
}

/// A same-API router confirms the schema only once its database file holds the
/// schema this binary supports; anything else defers to the local file check.
pub(super) fn router_confirms_schema(status: &serde_json::Value) -> bool {
    match (
        status["schema_file"].as_i64(),
        status["schema_supported"].as_i64(),
    ) {
        (Some(file), Some(supported)) => file == supported && supported == SCHEMA_VERSION,
        _ => false,
    }
}

/// Reads the storage schema without creating or migrating; `None` means absent.
pub(super) fn local_schema_version(database: &Path) -> Result<Option<i64>, BoardError> {
    match database.symlink_metadata() {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(unavailable(format!("board database: {error}"))),
    }
    let connection = rusqlite::Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|error| unavailable(format!("board database schema is unreadable: {error}")))?;
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map(Some)
        .map_err(|error| unavailable(format!("board database schema is unreadable: {error}")))
}

/// Fatal when a reachable router disagrees on API or database; `None` only
/// means no router answered. The returned status lets callers compare schema.
fn probe_router(
    database: &Path,
    deadline: QueryDeadline,
    exchange: &mut RouterExchange<'_>,
) -> Result<Option<serde_json::Value>, BoardError> {
    if deadline.expired() {
        return Err(deadline_error());
    }
    let Some(reply) = exchange(&["system".into(), "status".into()], deadline)? else {
        return Ok(None);
    };
    let status: serde_json::Value =
        serde_json::from_str(&reply).map_err(|_| unavailable("router returned invalid status"))?;
    if status["status"].as_str() != Some("ok")
        || status["board_api"].as_u64() != Some(u64::from(BOARD_API))
    {
        return Err(unavailable(
            "router board_api mismatch; restart trufflepig-system.service",
        ));
    }
    let reported = status["board_db"]
        .as_str()
        .map(Path::new)
        .ok_or_else(|| unavailable("router status omits board_db"))?;
    let same = database == reported
        || database
            .canonicalize()
            .ok()
            .zip(reported.canonicalize().ok())
            .is_some_and(|(local, router)| local == router);
    if !reported.is_absolute() || !same {
        return Err(unavailable(
            "web database differs from router board_db; restore the router database configuration",
        ));
    }
    Ok(Some(status))
}

pub(super) fn ingest_via(
    database: &Path,
    deadline: QueryDeadline,
    exchange: &mut RouterExchange<'_>,
) -> Result<serde_json::Value, BoardError> {
    // Ingestion runs on the router, which owns migration; schema is not consulted.
    if probe_router(database, deadline, exchange)?.is_none() {
        return Err(unavailable(
            "router is unavailable; start trufflepig system ensure",
        ));
    }
    if deadline.expired() {
        return Err(deadline_error());
    }
    let args = ["--json".into(), "board".into(), "ingest".into()];
    let reply = exchange(&args, deadline)?
        .ok_or_else(|| unavailable("router disappeared before ingestion"))?;
    serde_json::from_str(&reply)
        .map_err(|_| unavailable("router returned invalid ingestion receipt"))
}

pub(super) fn unavailable(message: impl Into<String>) -> BoardError {
    BoardError::new(BoardErrorCode::BoardUnavailable, message)
}
