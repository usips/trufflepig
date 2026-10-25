//! Diagnostics outcomes for failed requests, named by their error codes.
use crate::diagnostics::Outcome;
use tp_core::coded_error::{ErrorCode, named_codes};

/// The most specific outcome any code `error` names ([`named_codes`]: every
/// coded layer and codes inside relayed text), in the precedence budget,
/// stale, unavailable, invalid input; uncoded is a failure.
pub(crate) fn error_outcome(error: &anyhow::Error) -> Outcome {
    const PRECEDENCE: [Outcome; 4] = [
        Outcome::BudgetTooSmall,
        Outcome::Stale,
        Outcome::Unavailable,
        Outcome::InvalidInput,
    ];
    named_codes(error)
        .filter_map(code_outcome)
        .filter_map(|outcome| PRECEDENCE.iter().position(|ranked| *ranked == outcome))
        .min()
        .map_or(Outcome::Failure, |rank| PRECEDENCE[rank])
}

/// A code's outcome family by its spelling: `*budget_too_small*`, `*stale_*`,
/// `*unavailable*` or `*expired_*`, then `*invalid_*`, `unknown_command`, `usage`.
fn code_outcome(code: ErrorCode) -> Option<Outcome> {
    let name = code.as_str();
    if name.contains("budget_too_small") {
        Some(Outcome::BudgetTooSmall)
    } else if name.contains("stale_") {
        Some(Outcome::Stale)
    } else if name.contains("unavailable") || name.contains("expired_") {
        Some(Outcome::Unavailable)
    } else if name.contains("invalid_")
        || matches!(code, ErrorCode::UnknownCommand | ErrorCode::Usage)
    {
        Some(Outcome::InvalidInput)
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
