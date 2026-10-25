//! The trufflepig application layer: CLI, daemons, the system router,
//! workspaces, and diagnostics over the library crates in `crates/`.
//! Crate map: `docs/DESIGN.md#workspace-crates`.

#[macro_use]
extern crate tp_core;

pub(crate) mod cli;
pub(crate) mod daemon;
pub(crate) mod diagnostics;
pub(crate) mod system;
pub(crate) mod workspace;

// The library surface: the binary entry point.
pub use cli::emission::execute;
