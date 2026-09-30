//! Deterministic path prior for fused search: test and documentation paths
//! count less unless the query targets them or names the file. Demoted paths
//! stay eligible; see `docs/index-contract.md` (File ranking).
use super::{
    Query,
    concept_query::ConceptQuery,
    filename_score,
    path_class::{is_doc_pointer_path, is_test_path},
    query_terms,
};

/// Multiplier on each lane contribution of a demoted path. In reciprocal-rank
/// terms a demoted file at lane rank `r` scores like an undemoted one at rank
/// `2r + 61`: its best rank ties an undemoted file's rank 61.
const DEMOTED_PATH_WEIGHT: f64 = 0.5;
const TEST_TERMS: [&str; 6] = ["test", "tests", "testing", "spec", "fixture", "fixtures"];
const DOC_TERMS: [&str; 9] = [
    "doc",
    "docs",
    "documentation",
    "readme",
    "markdown",
    "md",
    "agents",
    "claude",
    "guide",
];

/// Which path classes a query demotes, and the terms that name a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PathPrior {
    demote_tests: bool,
    demote_docs: bool,
    /// Files the query names keep full weight; an identifier query instead
    /// ranks them in their own tier, where tests still count less.
    exempt_named: bool,
    terms: Vec<String>,
}

impl PathPrior {
    /// Demotes nothing.
    #[cfg(test)]
    pub(crate) const fn neutral() -> Self {
        Self {
            demote_tests: false,
            demote_docs: false,
            exempt_named: false,
            terms: Vec::new(),
        }
    }

    /// Demotes tests unless the text names tests or a `file:` component is a
    /// test directory; demotes docs unless the text names docs, `lang:text` is
    /// set, or `file:` selects a doc path. `kind:file` queries demote nothing.
    pub(crate) fn for_query(query: &Query) -> Self {
        let terms = query_terms(&query.text);
        let names = |class: &[&str]| terms.iter().any(|term| class.contains(&term.as_str()));
        let filter = query.path.to_ascii_lowercase();
        let components = || filter.split('/').filter(|part| !part.is_empty());
        let listing = query.kind == "file";
        Self {
            demote_tests: !listing
                && !names(&TEST_TERMS)
                && !is_test_path(&filter)
                && !components().any(|part| is_test_path(&format!("{part}/x"))),
            demote_docs: !listing
                && !names(&DOC_TERMS)
                && query.language != "text"
                && !is_doc_pointer_path(&filter)
                && !components().any(|part| matches!(part, "doc" | "docs")),
            exempt_named: ConceptQuery::parse(&query.text).identifier.is_none(),
            terms,
        }
    }

    /// Multiplier for one lane contribution of `path`; outside identifier
    /// queries, a file whose basename holds every query term is never demoted.
    pub(crate) fn weight(&self, path: &str) -> f64 {
        let demoted = (self.demote_tests && is_test_path(path))
            || (self.demote_docs && is_doc_pointer_path(path));
        if demoted && !(self.exempt_named && self.names(path)) {
            DEMOTED_PATH_WEIGHT
        } else {
            1.0
        }
    }

    /// The basename holds every query term.
    pub(crate) fn names(&self, path: &str) -> bool {
        !self.terms.is_empty() && filename_score(path, &self.terms) >= 2
    }

    /// The file stem is the query, up to case and separators (`tick_rate.rs`).
    pub(crate) fn names_exactly(&self, path: &str) -> bool {
        !self.terms.is_empty() && filename_score(path, &self.terms) == 3
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
        assert_eq!(
            prior("scripting kind:file").weight("docs/scripting.md"),
            1.0
        );
    }

    #[test]
    fn a_named_file_is_never_demoted_and_filters_match_components() {
        assert_eq!(prior("AGENTS.md").weight("crates/a/AGENTS.md"), 1.0);
        assert_eq!(prior("dev-http").weight("docs/dev-http.md"), 1.0);
        assert_eq!(prior("dev http server").weight("docs/dev-http.md"), 0.5);
        // `latest` and `document_ops` merely contain the words.
        assert_eq!(prior("tick file:src/latest/").weight("src/tests.rs"), 0.5);
        assert_eq!(
            prior("tick file:src/document_ops/").weight("src/document_ops/a.md"),
            0.5
        );
        assert!(prior("tick_rate").names_exactly("crates/core/src/tick_rate.rs"));
        // An identifier query tiers named files instead; tests among them count less.
        assert_eq!(prior("tick_rate").weight("src/tests/tick_rate.rs"), 0.5);
        assert!(!prior("tick_rate").names_exactly("crates/core/src/tick.rs"));
    }
}
