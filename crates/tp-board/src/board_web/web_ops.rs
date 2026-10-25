//! Board operations over HTTP: requests, ingest flights, and router probes.
mod ingest_flight;
mod router_probe;
#[cfg(test)]
mod tests;
mod web_request;

use super::WebStore;
use crate::board_protocol::BoardError;
pub(crate) use ingest_flight::{IngestFlight, relay_flight};
use router_probe::{ROUTER_MIGRATION_WAIT, ingest_via, startup_probe};
use std::{path::Path, time::Instant};
use tp_core::query_deadline::QueryDeadline;
pub(crate) use web_request::{WebRequest, execute};

pub(super) fn check_router_identity(runtime: &Path, database: &Path) -> Result<(), BoardError> {
    let context =
        tp_core::request_context::RequestContext::new(Some("web".into()), Some("human".into()));
    startup_probe(database, ROUTER_MIGRATION_WAIT, &mut |args, deadline| {
        tp_core::daemon_transport::request_by(runtime, args, &context, deadline)
            .map_err(BoardError::from)
    })
}

pub(super) fn relay_ingest(
    store: &WebStore,
    expires: Instant,
) -> Result<serde_json::Value, BoardError> {
    let config = store.config(expires)?;
    let context =
        tp_core::request_context::RequestContext::new(Some("web".into()), Some("human".into()));
    let deadline = QueryDeadline::after(expires.saturating_duration_since(Instant::now()));
    ingest_via(&config.db_path, deadline, &mut |args, deadline| {
        tp_core::daemon_transport::request_by(&store.runtime, args, &context, deadline)
            .map_err(BoardError::from)
    })
}
