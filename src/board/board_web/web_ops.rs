//! Board operations over HTTP: requests, ingest flights, and router probes.
mod ingest_flight;
mod router_probe;
#[cfg(test)]
mod tests;
mod web_request;

use super::WebStore;
use crate::board::board_protocol::BoardError;
use crate::daemon::deadline::QueryDeadline;
#[cfg(test)]
pub(crate) use ingest_flight::relay_flight_with_settle_hook;
pub(crate) use ingest_flight::{IngestFlight, relay_flight};
use router_probe::{ROUTER_MIGRATION_WAIT, ingest_via, startup_probe};
use std::{path::Path, time::Instant};
pub(crate) use web_request::{WebRequest, execute};

pub(super) fn check_router_identity(
    runtime: &Path,
    database: &Path,
    expires: Instant,
) -> Result<(), BoardError> {
    let context = crate::diagnostics::RequestContext::new(Some("web".into()), Some("human".into()));
    let deadline = QueryDeadline::after(expires.saturating_duration_since(Instant::now()));
    startup_probe(
        database,
        deadline,
        ROUTER_MIGRATION_WAIT,
        &mut |args, deadline| {
            crate::daemon::request_by(runtime, args, &context, deadline).map_err(BoardError::from)
        },
    )
}

pub(super) fn relay_ingest(
    store: &WebStore,
    expires: Instant,
) -> Result<serde_json::Value, BoardError> {
    let config = store.config(expires)?;
    let context = crate::diagnostics::RequestContext::new(Some("web".into()), Some("human".into()));
    let deadline = QueryDeadline::after(expires.saturating_duration_since(Instant::now()));
    ingest_via(&config.db_path, deadline, &mut |args, deadline| {
        crate::daemon::request_by(&store.runtime, args, &context, deadline)
            .map_err(BoardError::from)
    })
}
