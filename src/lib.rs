// First, so `coded!` is in textual scope for every later module.
#[macro_use]
pub mod coded_error;
pub(crate) mod background_process;
pub mod board;
pub mod cli;
pub mod daemon;
pub mod diagnostics;
pub mod extract;
pub mod history;
pub mod identity;
pub mod output;
pub mod probes;
pub(crate) mod process_capture;
pub mod query_deadline;
pub mod results;
pub mod search;
pub mod semantic;
pub mod source;
pub mod store;
pub mod system;
#[cfg(test)]
pub(crate) mod test_fixtures;
pub mod workspace;
