use super::*;
use crate::{output::OutputBudget, results};

pub(super) fn fixture(files: &[(&str, &[u8])]) -> (tempfile::TempDir, tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    for (path, bytes) in files {
        let path = root.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    let mut store = Store::open(root.path(), cache.path()).unwrap();
    store.index().unwrap();
    (root, cache, store)
}

#[test]
fn excluded_files_remain_visible_and_do_not_break_search() {
    let (_root, cache, mut store) =
        fixture(&[("lib.rs", b"fn hello() {}"), ("data.bin", b"\0binary")]);
    let set = search(&store, &Query::parse("hello").unwrap(), false, cache.path()).unwrap();
    assert!(set.hits.iter().any(|h| h.name == "hello"));
    let set = search(
        &store,
        &Query::parse("data.bin").unwrap(),
        false,
        cache.path(),
    )
    .unwrap();
    assert_eq!(set.hits.len(), 1);
    assert_eq!(set.hits[0].revision, None);
    let id = results::save(&mut store, set).unwrap();
    assert!(
        source::show(&store, &format!("{id}:1"), &OutputBudget::new(600).unwrap())
            .unwrap_err()
            .to_string()
            .contains("source_excluded")
    );
}

#[test]
fn lexical_search_covers_symbol_middles_gaps_docs_and_config() {
    let body = format!(
        "fn long_function() {{\n{}\nlet middle_needle=1;\n{}\n}}\n// gap_needle\n",
        "// padding\n".repeat(600),
        "// padding\n".repeat(600)
    );
    let (_root, cache, store) = fixture(&[
        ("lib.rs", body.as_bytes()),
        ("notes.md", b"documentation_needle"),
        ("config.toml", b"config_needle=1"),
    ]);
    for needle in [
        "middle_needle",
        "gap_needle",
        "documentation_needle",
        "config_needle",
    ] {
        let set = search(&store, &Query::parse(needle).unwrap(), false, cache.path()).unwrap();
        assert!(!set.hits.is_empty(), "{needle}");
    }
    let set = search(
        &store,
        &Query::parse("middle needle lang:rust").unwrap(),
        false,
        cache.path(),
    )
    .unwrap();
    assert!(!set.hits.is_empty());
}

#[test]
fn live_regex_finds_new_files_and_refuses_old_graph() {
    let (root, cache, mut store) = fixture(&[("lib.rs", b"fn alpha() {}")]);
    std::fs::write(root.path().join("new.rs"), "fn live_needle() {}\n").unwrap();
    let set = search(
        &store,
        &Query::parse("re:live_needle").unwrap(),
        false,
        cache.path(),
    )
    .unwrap();
    assert_eq!(set.hits[0].path, "new.rs");
    let id = results::save(&mut store, set).unwrap();
    assert!(
        context(&store, &format!("{id}:1"), &OutputBudget::new(600).unwrap())
            .unwrap_err()
            .to_string()
            .contains("stale_result")
    );
    let set = search(
        &store,
        &Query::parse("re:live_needle kind:struct").unwrap(),
        false,
        cache.path(),
    )
    .unwrap();
    assert!(set.hits.is_empty());
}

#[test]
fn escaped_file_handles_read_current_contents() {
    let (_root, cache, mut store) = fixture(&[("odd:1-2\n%.md", b"unique file content\n")]);
    let set = search(&store, &Query::parse("odd").unwrap(), false, cache.path()).unwrap();
    let path = set
        .hits
        .iter()
        .find(|h| h.kind == "file")
        .unwrap()
        .path
        .clone();
    assert!(path.contains("%3A"));
    assert!(path.contains("%0A"));
    let id = results::save(&mut store, set).unwrap();
    let set = results::load(&store, &id).unwrap();
    let hit = set.hits.iter().find(|h| h.kind == "file").unwrap();
    let budget = OutputBudget::new(600).unwrap();
    assert!(
        source::show(&store, &hit.handle, &budget)
            .unwrap()
            .contains("unique file content")
    );
    assert!(
        source::show(&store, &format!("path:{path}"), &budget)
            .unwrap()
            .contains("unique file content")
    );
}

#[test]
fn duplicate_definitions_remain_distinct_and_ranking_stable() {
    let (_root, cache, store) = fixture(&[
        ("a.rs", b"fn duplicate() {}"),
        ("b.rs", b"fn duplicate() {}"),
    ]);
    let query = Query::parse("sym:duplicate").unwrap();
    let a = search(&store, &query, false, cache.path()).unwrap();
    let b = search(&store, &query, false, cache.path()).unwrap();
    assert_eq!(a.hits.len(), 2);
    assert_ne!(a.hits[0].path, a.hits[1].path);
    assert_eq!(
        serde_json::to_string(&a).unwrap(),
        serde_json::to_string(&b).unwrap()
    );
}

#[test]
fn repeated_regions_in_one_file_cannot_hide_other_files() {
    let crowded = (0..1_100)
        .map(|index| format!("fn needle_{index}() {{}}\n"))
        .collect::<String>();
    let (_root, cache, store) = fixture(&[
        ("crowded.rs", crowded.as_bytes()),
        ("other.rs", b"fn needle() {}"),
    ]);
    let set = search(
        &store,
        &Query::parse("needle").unwrap(),
        false,
        cache.path(),
    )
    .unwrap();
    assert!(set.hits.iter().any(|hit| hit.path == "crowded.rs"));
    assert!(set.hits.iter().any(|hit| hit.path == "other.rs"));
}

#[test]
fn filename_basename_match_ranks_before_partial_path_match() {
    let (_root, cache, store) = fixture(&[
        ("content/airlock.dm", b"unrelated"),
        ("content/programmable_airlock.dm", b"unrelated"),
        ("docs/airlock_notes.md", b"unrelated"),
    ]);
    let set = search(
        &store,
        &Query::parse("airlock").unwrap(),
        false,
        cache.path(),
    )
    .unwrap();
    assert_eq!(set.hits[0].path, "content/airlock.dm");
}

#[test]
fn exact_and_regex_queries_retain_same_file_occurrences() {
    let (_root, cache, store) = fixture(&[
        ("defs.rs", b"fn target() {}\nfn target() {}\n"),
        ("matches.txt", b"target target\nleading target\ntarget\n"),
    ]);
    let exact = search(
        &store,
        &Query::parse("sym:target").unwrap(),
        false,
        cache.path(),
    )
    .unwrap();
    assert_eq!(
        exact
            .hits
            .iter()
            .filter(|hit| hit.path == "defs.rs")
            .count(),
        2
    );

    // Regex hits are one per matching line, and anchors match at each line.
    let lines = |text: &str| -> Vec<usize> {
        search(&store, &Query::parse(text).unwrap(), false, cache.path())
            .unwrap()
            .hits
            .iter()
            .filter(|hit| hit.path == "matches.txt")
            .map(|hit| hit.start_line)
            .collect()
    };
    assert_eq!(lines("re:target"), [1, 2, 3]);
    assert_eq!(lines("re:^target"), [1, 3]);
    assert_eq!(lines("re:target$"), [1, 2, 3]);
}

#[test]
fn filter_only_queries_and_reference_identities_are_useful() {
    let (_root, cache, store) =
        fixture(&[("lib.rs", b"fn alpha() {}\nfn caller() { alpha(); }\n")]);
    let set = search(
        &store,
        &Query::parse("file:lib.rs kind:function").unwrap(),
        false,
        cache.path(),
    )
    .unwrap();
    assert!(set.hits.iter().any(|h| h.name == "alpha"));
    let set = references(&store, "alpha").unwrap();
    let call = set.hits.iter().find(|h| h.kind == "call").unwrap();
    assert_eq!(call.resolution.as_deref(), Some("resolved"));
    assert_eq!(call.target.as_ref().unwrap().path, "lib.rs");
    assert_eq!(call.target.as_ref().unwrap().name, "alpha");
}

#[test]
fn rerank_gate_excludes_exact_and_regex_queries() {
    let (_root, cache, store) = fixture(&[("a.md", b"marker\n")]);
    let mut session = crate::semantic::SemanticSession::default();
    for text in ["sym:marker", "re:marker"] {
        let query = Query::parse(text).unwrap();
        let mut trace = telemetry::RetrievalTrace::disabled();
        let result = search_with_session(
            &store,
            &query,
            false,
            true,
            cache.path(),
            &mut session,
            &mut trace,
        )
        .unwrap();
        assert!(
            result.coverage.get("rerank_status").is_none(),
            "{text} set rerank_status: {:?}",
            result.coverage.get("rerank_status")
        );
    }
}

#[test]
fn show_symbol_reads_the_best_declaration_and_lists_the_rest() {
    let (_root, _cache, store) = fixture(&[
        ("a.rs", b"mod refill {}\nfn caller(refill: u8) {}\n"),
        (
            "b.rs",
            b"/// Refills.\npub fn refill() {\n    let tokens = 1;\n}\n",
        ),
    ]);
    let budget = crate::output::OutputBudget::new(4000)
        .unwrap()
        .with_format(crate::output::OutputFormat::Lines);
    let text = crate::source::show(&store, "sym:refill", &budget).unwrap();
    assert!(text.starts_with("b.rs "), "{text}");
    assert!(text.contains("pub fn refill() {") && text.contains("let tokens = 1;"));
    assert!(text.contains("verified: true"));
    assert!(text.contains("definitions: 3"), "{text}");
    assert!(text.contains("also: a.rs:1-1 module"), "{text}");
    let scoped = crate::source::show(&store, "sym:refill file:a.rs kind:module", &budget).unwrap();
    assert!(
        scoped.starts_with("a.rs ") && scoped.contains("definitions: 1"),
        "{scoped}"
    );
    let missing = crate::source::show(&store, "sym:absent", &budget).unwrap_err();
    assert!(missing.to_string().starts_with("no_definition:"));
}

#[test]
fn map_of_one_file_lists_its_functions_but_a_prefix_lists_types_only() {
    let (_root, _cache, store) =
        fixture(&[("src/lib.rs", b"pub struct Bucket;\nfn refill() {}\n")]);
    let names = |path: &str| -> Vec<String> {
        map(&store, path)
            .unwrap()
            .hits
            .into_iter()
            .map(|hit| hit.name)
            .collect()
    };
    assert!(names("src/lib.rs").contains(&"refill".to_owned()));
    assert!(!names("src/").contains(&"refill".to_owned()));
    assert!(names("src/").contains(&"Bucket".to_owned()));
}
