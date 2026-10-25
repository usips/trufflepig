//! The repository index: the SQLite store and its publication, current
//! source reads, search and ranking, typed results, semantic preparation,
//! and index probes. These modules reference each other and form one crate.
//! Contracts: `docs/index-contract.md`, `docs/retrieval-contract.md`.

#[macro_use]
extern crate tp_core;

pub mod probes;
pub mod results;
pub mod search;
pub mod semantic_preparation;
pub mod source;
pub mod store;
