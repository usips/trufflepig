use super::*;
use crate::search::{search, tests::fixture};

fn identifier(name: &str, qualifier: Option<&str>) -> Option<IdentifierToken> {
    Some(IdentifierToken {
        name: name.into(),
        qualifier: qualifier.map(Into::into),
    })
}

fn paths(input: &str, files: &[(&str, &[u8])]) -> Vec<String> {
    let (_root, cache, store) = fixture(files);
    let set = search(&store, &Query::parse(input).unwrap(), false, cache.path()).unwrap();
    set.hits.into_iter().map(|hit| hit.path).collect()
}

#[test]
fn identifier_shape_needs_underscore_path_camel_case_or_digits() {
    for (text, expected) in [
        ("molar_mass", identifier("molar_mass", None)),
        ("ModOwner", identifier("ModOwner", None)),
        ("speed_mm_tick", identifier("speed_mm_tick", None)),
        ("u32", identifier("u32", None)),
        ("harvest::record", identifier("record", Some("harvest"))),
        ("a::b::Coord", identifier("Coord", Some("b"))),
        ("Actor", None),
        ("owner", None),
        ("2024", None),
        ("unknown item slot", None),
        ("a:b", None),
        ("::b", None),
    ] {
        assert_eq!(ConceptQuery::parse(text).identifier, expected, "{text}");
    }
}

#[test]
fn multi_word_text_adds_phrase_and_all_terms_and_literals_run_phrase_only() {
    let words = ConceptQuery::parse("unknown item-slot");
    assert_eq!(words.phrase.as_deref(), Some("\"unknown item slot\""));
    assert_eq!(
        words.all_terms.as_deref(),
        Some("\"unknown\" AND \"item\" AND \"slot\"")
    );
    assert_eq!(
        words.any_terms.as_deref(),
        Some("\"unknown\" OR \"item\" OR \"slot\"")
    );
    let single = ConceptQuery::parse("awake");
    assert_eq!((single.phrase, single.all_terms), (None, None));
    let literal = ConceptQuery::parse("\"outside an occurrence\"");
    assert!(literal.literal && literal.identifier.is_none());
    assert_eq!(literal.phrase.as_deref(), Some("\"outside an occurrence\""));
    assert_eq!((literal.any_terms, literal.all_terms), (None, None));
}

#[test]
fn identifier_query_ranks_definition_and_uses_before_files_lacking_the_token() {
    let found = paths(
        "molar_mass",
        &[
            ("src/molar_mass.ron", b"molar mass: 12\nmolar mass: 14\n"),
            (
                "src/substances.rs",
                b"pub struct Substance { mass: f32 }\nimpl Substance {\n    pub fn molar_mass(&self) -> f32 { self.mass }\n}\n",
            ),
            (
                "src/reader.rs",
                b"use crate::substances::Substance;\nfn read(s: &Substance) -> f32 { s.molar_mass() }\n",
            ),
            (
                "src/tests.rs",
                b"fn molar_mass_reads() { assert_eq!(substance().molar_mass(), 1.0); molar_mass(); }\n",
            ),
        ],
    );
    assert_eq!(
        found,
        [
            "src/substances.rs",
            "src/reader.rs",
            "src/tests.rs",
            "src/molar_mass.ron"
        ]
    );
}

#[test]
fn phrase_match_leads_scattered_terms_and_its_line_is_the_snippet() {
    let scattered = "// unknown slot: an item may be unknown; each slot holds one item\n".repeat(8);
    let files: [(&str, &[u8]); 3] = [
        ("src/scatter.rs", scattered.as_bytes()),
        (
            "src/sim/tests/item_slots.rs",
            b"fn unknown() { assert_eq!(fault(), \"unknown item slot\"); }\n",
        ),
        (
            "src/sim/item_slots.rs",
            b"impl Display for Fault {\n    fn fmt(&self, f: &mut Formatter) -> Result {\n        f.write_str(\"unknown item slot\")\n    }\n}\n",
        ),
    ];
    let (_root, cache, store) = fixture(&files);
    let set = search(
        &store,
        &Query::parse("unknown item slot").unwrap(),
        false,
        cache.path(),
    )
    .unwrap();
    let found: Vec<_> = set.hits.iter().map(|hit| hit.path.as_str()).collect();
    assert_eq!(
        found,
        [
            "src/sim/item_slots.rs",
            "src/sim/tests/item_slots.rs",
            "src/scatter.rs"
        ]
    );
    assert_eq!(set.hits[0].provenance.as_deref(), Some("phrase"));
    let snippet = set.hits[0].snippet.as_ref().unwrap();
    assert_eq!(snippet.text, "f.write_str(\"unknown item slot\")");

    let literal = search(
        &store,
        &Query::parse("\"unknown item slot\"").unwrap(),
        false,
        cache.path(),
    )
    .unwrap();
    let found: Vec<_> = literal.hits.iter().map(|hit| hit.path.as_str()).collect();
    assert_eq!(
        found,
        ["src/sim/item_slots.rs", "src/sim/tests/item_slots.rs"]
    );
}

#[test]
fn test_files_lead_when_the_query_asks_for_tests() {
    let files: [(&str, &[u8]); 2] = [
        ("src/sim/tests.rs", b"fn refill_slot_test() { refill(); }\n"),
        ("src/sim/refill.rs", b"pub fn refill() { slot(); }\n"),
    ];
    assert_eq!(paths("refill slot", &files)[0], "src/sim/refill.rs");
    assert_eq!(paths("refill slot test", &files)[0], "src/sim/tests.rs");
}
