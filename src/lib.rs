#[macro_use]
extern crate tp_core;

pub(crate) mod cli;
pub(crate) mod daemon;
pub(crate) mod diagnostics;
pub(crate) mod system;
pub(crate) mod workspace;

// The library surface: the binary entry point.
pub use cli::emission::execute;
