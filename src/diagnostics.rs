//! Bounded local observations with explicit sessions and delivery evidence.

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
pub use audit::AuditReport;
pub use delivery::{DeliveryFailure, DeliveryReceipt, emit_response};
pub use journal::{DiagnosticStore, RecordStatus};
pub use records::{
    DiagnosticsMode, EmittedIdentity, EventStage, Operation, Outcome, RequestContext, RequestEvent,
};
