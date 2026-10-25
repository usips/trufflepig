#[macro_use]
extern crate tp_core;

pub(crate) mod board;
pub(crate) mod cli;
pub(crate) mod daemon;
pub(crate) mod diagnostics;
pub(crate) mod history;
pub(crate) mod probes;
pub(crate) mod results;
pub(crate) mod search;
pub(crate) mod semantic;
pub(crate) mod semantic_preparation;
pub(crate) mod source;
pub(crate) mod store;
pub(crate) mod system;
pub(crate) mod workspace;

// The library surface: the binary entry point, plus the items the
// integration tests and the semantic benchmark example use.
pub use board::{BOARD_API, SCHEMA_VERSION};
pub use cli::emission::execute;
#[cfg(feature = "semantic")]
pub use semantic::{SemanticInferenceEngine, SemanticProviderConfig, VerifiedAssetDigests};
