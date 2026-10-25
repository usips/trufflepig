use super::*;
use crate::coded_error::{CodedContext, relayed_error};
use anyhow::anyhow;

fn relayed(text: &str) -> anyhow::Error {
    relayed_error(text.to_owned())
}

#[test]
fn codes_name_outcomes_by_family() {
    for (error, outcome) in [
        (
            anyhow!(coded!(BudgetTooSmall, "need 900")),
            Outcome::BudgetTooSmall,
        ),
        (
            anyhow!(coded!(StaleSource, "src/lib.rs changed")),
            Outcome::Stale,
        ),
        (anyhow!(coded!(StaleHandle, "h:1")), Outcome::Stale),
        (
            anyhow!(coded!(DaemonUnavailable, "no socket")),
            Outcome::Unavailable,
        ),
        (anyhow!(coded!(ExpiredResult, "h:1")), Outcome::Unavailable),
        (anyhow!(coded!(InvalidLimit, "0")), Outcome::InvalidInput),
        (
            anyhow!(coded!(UnknownCommand, "frob")),
            Outcome::InvalidInput,
        ),
        (anyhow!(coded!(Usage, "show TARGET")), Outcome::InvalidInput),
        (
            anyhow!(coded!(TimedOut, "query deadline expired")),
            Outcome::Failure,
        ),
        (anyhow!("uncoded failure"), Outcome::Failure),
    ] {
        assert_eq!(error_outcome(&error), outcome, "{error:#}");
    }
}

#[test]
fn the_most_specific_code_in_the_chain_wins() {
    let error = anyhow!(coded!(InvalidLimit, "0"))
        .with_coded_context(|| coded!(StaleSource, "changed"))
        .with_coded_context(|| coded!(TimedOut, "query deadline expired"));
    assert_eq!(error_outcome(&error), Outcome::Stale);
    let relayed = relayed("daemon: budget_too_small: need 900");
    assert_eq!(error_outcome(&relayed), Outcome::BudgetTooSmall);
}

#[test]
fn codes_inside_relayed_text_name_outcomes_by_precedence() {
    for (text, outcome) in [
        (
            "daemon: timed_out: waited 5 s; index_cache_unavailable",
            Outcome::Unavailable,
        ),
        (
            "daemon: timed_out: member: index_cache_unavailable: cannot open",
            Outcome::Unavailable,
        ),
        (
            "daemon: semantic_fallback_output_too_large: does not fit: budget_too_small: need 900",
            Outcome::BudgetTooSmall,
        ),
        (
            "load page: budget_too_small: need 900",
            Outcome::BudgetTooSmall,
        ),
        (
            "search 'invalid_limit usage: stale_source' found nothing",
            Outcome::Stale,
        ),
        (
            "daemon: internal_error: bad usage: x",
            Outcome::InvalidInput,
        ),
    ] {
        assert_eq!(error_outcome(&relayed(text)), outcome, "{text}");
    }
}

#[test]
fn family_spellings_inside_longer_codes_still_classify() {
    let error = anyhow!(coded!(SemanticFallbackBudgetTooSmall, "need 900"));
    assert_eq!(error_outcome(&error), Outcome::BudgetTooSmall);
}

#[test]
fn free_text_that_is_not_a_code_names_no_outcome() {
    for text in [
        "Resource temporarily unavailable (os error 11)",
        "see usage below; stale_sources were skipped",
        "daemon: timed_out: Stale_source xinvalid_limit",
    ] {
        assert_eq!(error_outcome(&relayed(text)), Outcome::Failure, "{text}");
    }
    let local = anyhow!(coded!(TimedOut, "mentions index_cache_unavailable"));
    assert_eq!(error_outcome(&local), Outcome::Failure);
}
