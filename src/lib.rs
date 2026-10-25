// First, so `coded!` is in textual scope for every later module.
#[macro_use]
pub mod coded_error;
pub(crate) mod background_process;
pub mod board;
pub mod cli;
pub mod daemon;
pub mod daemon_transport;
pub mod diagnostics;
pub mod extract;
pub mod git_runner;
pub mod history;
pub mod identity;
pub mod line_diff;
pub mod output;
pub mod path_codec;
pub mod probes;
pub(crate) mod process_capture;
pub mod query_deadline;
pub mod request_context;
pub mod request_pool;
pub mod results;
pub mod runtime_paths;
pub mod search;
pub mod semantic;
pub mod semantic_preparation;
pub mod source;
pub mod store;
pub mod system;
#[cfg(test)]
pub(crate) mod test_fixtures;
pub mod workspace;
pub mod workspace_config;
