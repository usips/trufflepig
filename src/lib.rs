#[macro_use]
extern crate tp_core;

pub(crate) mod board;
pub(crate) mod cli;
pub(crate) mod daemon;
pub(crate) mod diagnostics;
pub(crate) mod history;
pub(crate) mod system;
pub(crate) mod workspace;

// The library surface: the binary entry point, plus the items the
// integration tests use.
pub use board::{BOARD_API, SCHEMA_VERSION};
pub use cli::emission::execute;
