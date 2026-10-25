//! Runtime services every trufflepig crate shares: typed error codes,
//! identities, output envelopes, deadlines, request context, process and
//! Git runners, runtime paths, and the daemon socket transport.
//! Contract: `docs/DESIGN.md#workspace-crates`.

// First, so `coded!` is in textual scope for every later module.
#[macro_use]
pub mod coded_error;
pub mod background_process;
pub mod build_identity;
pub mod daemon_transport;
pub mod git_runner;
pub mod identity;
pub mod line_diff;
pub mod output;
pub mod path_codec;
pub mod process_capture;
pub mod query_deadline;
pub mod request_context;
pub mod request_pool;
pub mod runtime_paths;
#[cfg(any(test, feature = "test-support"))]
pub mod test_fixtures;
pub mod workspace_config;
