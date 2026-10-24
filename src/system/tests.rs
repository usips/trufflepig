mod board_marker_tests;
mod managed_unit_tests;
mod router_spawn_tests;
mod router_start_tests;
mod router_status_tests;

use super::board_runtime::{BoardDatabaseMarker, board_database_marker_matches};
use super::*;
use std::path::Path;

fn ensure_with<'run>(
    start_wait: Duration,
    start: impl FnOnce(QueryDeadline) -> Result<managed_unit::RouterStart<'run>>,
) -> Result<()> {
    let deadline = QueryDeadline::after(start_wait);
    ensure_until(deadline, start_wait, start)
}
