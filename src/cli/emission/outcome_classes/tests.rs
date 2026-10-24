use super::*;
use crate::coded_error::CodedContext;
use anyhow::anyhow;

fn relayed(text: &str) -> anyhow::Error {
    CodedError::from_relayed(text.to_owned()).map_or_else(anyhow::Error::msg, anyhow::Error::from)
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
fn free_text_never_names_an_outcome() {
    for text in [
        "search 'invalid_limit usage: stale_source' found nothing",
        "Resource temporarily unavailable (os error 11)",
        "load page: budget_too_small: quoted",
    ] {
        assert_eq!(error_outcome(&relayed(text)), Outcome::Failure, "{text}");
    }
}
