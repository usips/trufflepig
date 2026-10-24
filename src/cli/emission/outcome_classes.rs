//! Diagnostics outcomes for failed requests, named by their error codes.
use crate::coded_error::{CodedError, ErrorCode};
use crate::diagnostics::Outcome;

/// The most specific outcome any code in `error`'s chain names, in the
/// precedence budget, stale, unavailable, invalid input; uncoded is a failure.
pub(super) fn error_outcome(error: &anyhow::Error) -> Outcome {
    const PRECEDENCE: [Outcome; 4] = [
        Outcome::BudgetTooSmall,
        Outcome::Stale,
        Outcome::Unavailable,
        Outcome::InvalidInput,
    ];
    let named: Vec<Outcome> = error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<CodedError>())
        .filter_map(|coded| code_outcome(coded.code()))
        .collect();
    PRECEDENCE
        .into_iter()
        .find(|outcome| named.contains(outcome))
        .unwrap_or(Outcome::Failure)
}

fn code_outcome(code: ErrorCode) -> Option<Outcome> {
    let name = code.as_str();
    if code == ErrorCode::BudgetTooSmall {
        Some(Outcome::BudgetTooSmall)
    } else if name.starts_with("stale_") {
        Some(Outcome::Stale)
    } else if name.contains("unavailable") || name.starts_with("expired_") {
        Some(Outcome::Unavailable)
    } else if name.starts_with("invalid_")
        || matches!(code, ErrorCode::UnknownCommand | ErrorCode::Usage)
    {
        Some(Outcome::InvalidInput)
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
