//! Bounded local observations keep diagnostic baselines separate from actor identity.
//! Native actor sessions do not become diagnostic baselines implicitly.

mod appender;
mod audit;
mod delivery;
mod journal;
mod ledger;
mod records;
pub mod sessions;
#[cfg(test)]
mod tests;

pub use appender::{DiagnosticQueue, best_effort_record};
pub use delivery::{DeliveryReceipt, emit_response};
pub use journal::{DiagnosticStore, RecordStatus};
pub use records::{DiagnosticsMode, EmittedIdentity, EventStage, Operation, Outcome, RequestEvent};
