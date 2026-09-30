//! Deterministic path prior for fused search: test and documentation paths
//! count half unless the query targets them. Demoted paths stay eligible; see
//! `docs/retrieval-contract.md` (Retrieval).
use super::{
    Query,
    path_class::{is_doc_pointer_path, is_test_path},
};

/// Score multiplier applied to a demoted path's fused contributions.
const DEMOTED_PATH_WEIGHT: f64 = 0.5;
const TEST_TERMS: [&str; 6] = ["test", "tests", "testing", "spec", "fixture", "fixtures"];
const DOC_TERMS: [&str; 5] = ["doc", "docs", "documentation", "readme", "markdown"];

/// Which path classes a query demotes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PathPrior {
    demote_tests: bool,
    demote_docs: bool,
}

impl PathPrior {
    /// Demotes nothing.
    #[cfg(test)]
    pub(crate) const NEUTRAL: Self = Self {
        demote_tests: false,
        demote_docs: false,
    };

    /// Demotes tests unless the text names tests or `file:` selects a test
    /// path; demotes docs unless the text names docs, `lang:text` is set, or
    /// `file:` selects a documentation path.
    pub(crate) fn for_query(query: &Query) -> Self {
        let words = query
            .text
            .split(|ch: char| !ch.is_alphanumeric())
            .map(str::to_lowercase)
            .collect::<Vec<_>>();
        let names = |terms: &[&str]| words.iter().any(|word| terms.contains(&word.as_str()));
        let filter = query.path.to_lowercase();
        Self {
            demote_tests: !names(&TEST_TERMS)
                && !(is_test_path(&filter) || filter.contains("test")),
            demote_docs: !names(&DOC_TERMS)
                && query.language != "text"
                && !(is_doc_pointer_path(&filter) || filter.contains("doc")),
        }
    }

    /// Multiplier for one fused file score.
    pub(crate) fn weight(&self, path: &str) -> f64 {
        if (self.demote_tests && is_test_path(path))
            || (self.demote_docs && is_doc_pointer_path(path))
        {
            DEMOTED_PATH_WEIGHT
        } else {
            1.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prior(input: &str) -> PathPrior {
        PathPrior::for_query(&Query::parse(input).unwrap())
    }

    #[test]
    fn prior_halves_tests_and_docs_unless_the_query_targets_them() {
        let plain = prior("unknown item slot");
        assert_eq!(plain.weight("src/sim/item_slots.rs"), 1.0);
        assert_eq!(plain.weight("src/sim/tests/item_slots.rs"), 0.5);
        assert_eq!(plain.weight("CLAUDE.md"), 0.5);

        assert_eq!(prior("sentence_tests").weight("src/tests.rs"), 1.0);
        assert_eq!(
            prior("item slot file:src/sim/tests/").weight("src/sim/tests/a.rs"),
            1.0
        );
        assert_eq!(prior("scripting docs").weight("docs/scripting.md"), 1.0);
        assert_eq!(
            prior("permissions lang:text").weight("docs/scripting.md"),
            1.0
        );
        assert_eq!(prior("permissions lang:text").weight("src/tests.rs"), 0.5);
        assert_eq!(prior("permissions file:docs/").weight("docs/a.md"), 1.0);
    }
}
