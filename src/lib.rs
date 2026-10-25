// First, so `coded!` is in textual scope for every later module.
#[macro_use]
pub(crate) mod coded_error;
pub(crate) mod background_process;
pub(crate) mod board;
pub(crate) mod cli;
pub(crate) mod daemon;
pub(crate) mod daemon_transport;
pub(crate) mod diagnostics;
pub(crate) mod git_runner;
pub(crate) mod history;
pub(crate) mod identity;
pub(crate) mod line_diff;
pub(crate) mod output;
pub(crate) mod path_codec;
pub(crate) mod probes;
pub(crate) mod process_capture;
pub(crate) mod query_deadline;
pub(crate) mod request_context;
pub(crate) mod request_pool;
pub(crate) mod results;
pub(crate) mod runtime_paths;
pub(crate) mod search;
pub(crate) mod semantic;
pub(crate) mod semantic_preparation;
pub(crate) mod source;
pub(crate) mod store;
pub(crate) mod system;
#[cfg(test)]
pub(crate) mod test_fixtures;
pub(crate) mod workspace;
pub(crate) mod workspace_config;

// The library surface: the binary entry point, plus the items the
// integration tests and the semantic benchmark example use.
pub use board::{BOARD_API, SCHEMA_VERSION};
pub use cli::emission::execute;
pub use daemon_transport::{PROXY_REPLY_WAIT, request_by, stop};
pub use query_deadline::QueryDeadline;
pub use request_context::RequestContext;
#[cfg(feature = "semantic")]
pub use semantic::{SemanticInferenceEngine, SemanticProviderConfig, VerifiedAssetDigests};
