use super::*;
use crate::{
    output::{OutputBudget, OutputFormat},
    search::{PathFilter, search, tests::fixture},
};

fn show(store: &Store, target: &str) -> Result<String> {
    let budget = OutputBudget::new(4000)
        .unwrap()
        .with_format(OutputFormat::Lines);
    crate::source::show(store, target, &budget)
}

fn locations(store: &Store, text: &str, origin: &InvocationDirectory) -> Vec<String> {
    crate::search::definitions(store, &Query::parse(text).unwrap(), origin)
        .unwrap()
        .hits
        .into_iter()
        .map(|hit| format!("{}:{}", hit.path, hit.kind))
        .collect()
}

#[test]
fn test_paths_and_invocation_depth_follow_conventions() {
    for path in [
        "tests/a.rs",
        "src/store/tests.rs",
        "src/a_test.go",
        "pkg/test_util.py",
        "web/a.spec.ts",
    ] {
        assert!(is_test_path(path), "{path}");
    }
    // Content packs keep production code under `fixtures/` (see `path_class`).
    for path in [
        "src/testing.rs",
        "src/attest.rs",
        "src/contest/mod.rs",
        "evaluation/fixtures/x.dm",
    ] {
        assert!(!is_test_path(path), "{path}");
    }
    let origin = InvocationDirectory::within(Path::new("/r"), Path::new("/r/crates/a/src"));
    assert_eq!(origin.shared_depth("crates/a/src/lib.rs"), 3);
    assert_eq!(origin.shared_depth("crates/b/src/lib.rs"), 1);
    assert_eq!(
        InvocationDirectory::within(Path::new("/r"), Path::new("/elsewhere")),
        InvocationDirectory::root()
    );
}

#[test]
fn show_prefers_implementations_over_test_helpers() {
    let (_root, _cache, store) = fixture(&[
        (
            "src/a.rs",
            b"#[cfg(test)]\nmod tests {\n    fn build() {}\n}\n",
        ),
        ("src/engine/tests.rs", b"fn build() {}\n"),
        ("src/zeta.rs", b"pub fn build() -> u8 { 1 }\n"),
        ("tests/it.rs", b"fn build() {}\n"),
    ]);
    let text = show(&store, "sym:build").unwrap();
    assert!(text.starts_with("src/zeta.rs "), "{text}");
    assert!(text.contains("definitions: 4\n"), "{text}");
}

#[test]
fn show_ranks_namesakes_nearest_to_the_invocation_directory() {
    let (root, _cache, store) = fixture(&[
        ("crates/a/src/lib.rs", b"pub fn run() {}\n"),
        ("crates/b/src/lib.rs", b"pub fn run() {}\n"),
    ]);
    let near_b = InvocationDirectory::within(root.path(), &root.path().join("crates/b/src"));
    assert_eq!(
        locations(&store, "sym:run", &near_b)[0],
        "crates/b/src/lib.rs:function"
    );
    assert_eq!(
        locations(&store, "sym:run", &InvocationDirectory::root())[0],
        "crates/a/src/lib.rs:function"
    );
}

#[test]
fn show_counts_imports_apart_and_follows_aliases_and_reexports() {
    let (_root, _cache, store) = fixture(&[
        (
            "src/lib.rs",
            b"pub use crate::store::Store as Db;\nuse serde::Serialize as Ser;\n",
        ),
        ("src/a.rs", b"use crate::store::Store;\nfn f(_: Store) {}\n"),
        ("src/store.rs", b"pub struct Store;\n"),
    ]);
    let text = show(&store, "sym:Store").unwrap();
    assert!(text.starts_with("src/store.rs "), "{text}");
    assert!(text.contains("definitions: 1 (+1 imports)"), "{text}");
    assert!(text.contains("also: src/a.rs:1-1 import"), "{text}");

    let alias = show(&store, "sym:Db").unwrap();
    assert!(
        alias.starts_with("src/store.rs ") && alias.contains("pub struct Store;"),
        "{alias}"
    );
    assert!(
        alias.contains("via: re-export of crate::store::Store at src/lib.rs:1\n"),
        "{alias}"
    );
    assert!(alias.contains("definitions: 0 (+1 imports)"), "{alias}");

    let filtered = show(&store, "sym:Store file:src/a.rs").unwrap();
    assert!(filtered.starts_with("src/store.rs "), "{filtered}");
    assert!(
        filtered.contains("via: import of crate::store::Store at src/a.rs:1"),
        "{filtered}"
    );

    let external = show(&store, "sym:Ser").unwrap();
    assert!(external.starts_with("src/lib.rs "), "{external}");
    assert!(
        external.contains("import of serde::Serialize at src/lib.rs:2 (declaration not indexed)"),
        "{external}"
    );
}

#[test]
fn qualified_names_select_impl_owners_and_module_paths() {
    let (_root, cache, store) = fixture(&[
        (
            "src/store.rs",
            b"pub struct Store;\nimpl Store {\n    pub fn open() {}\n}\npub struct Other;\nimpl Other {\n    fn open() {}\n}\nimpl std::fmt::Display for Store {\n    fn fmt(&self) {}\n}\n",
        ),
        ("src/sim/harvest.rs", b"pub fn record() {}\n"),
        ("src/ledger.rs", b"pub fn record() {}\n"),
    ]);
    let root = InvocationDirectory::root();
    let lines = |text: &str| -> Vec<usize> {
        crate::search::definitions(&store, &Query::parse(text).unwrap(), &root)
            .unwrap()
            .hits
            .iter()
            .map(|hit| hit.start_line)
            .collect()
    };
    assert_eq!(lines("sym:Store::open"), [3]);
    assert_eq!(lines("sym:Other::open"), [7]);
    assert_eq!(lines("sym:crate::store::Store::open"), [3]);
    assert_eq!(lines("sym:Display::fmt"), [10]);
    assert_eq!(lines("sym:Store::fmt"), [10]);
    assert_eq!(
        locations(&store, "sym:harvest::record", &root),
        ["src/sim/harvest.rs:function"]
    );
    assert!(lines("sym:Missing::open").is_empty());
    let found = search(
        &store,
        &Query::parse("sym:Store::open").unwrap(),
        false,
        cache.path(),
    )
    .unwrap();
    assert_eq!(found.hits.len(), 1);
    assert!(
        show(&store, "sym:Store::open")
            .unwrap()
            .contains("pub fn open() {}")
    );
}

#[test]
fn qualified_declarations_apply_the_owner_filter_before_the_result_cap() {
    let (_root, _cache, store) = fixture(&[(
        "src/a.rs",
        b"impl A0 { fn open() {} }\nimpl A1 { fn open() {} }\nimpl A { fn open() {} }\n",
    )]);
    let paths = PathFilter::default().bind(&store.conn).unwrap();
    let found =
        qualified_hits_with_limit(&store, &QualifiedName::parse("A::open"), &paths, "", "", 1)
            .unwrap();
    assert_eq!(found.hits.len(), 1, "{found:?}");
    assert_eq!(found.hits[0].container.as_deref(), Some("impl A"));
    assert!(!found.truncated);
}

#[test]
fn show_filters_from_separate_words_and_no_definition_names_them() {
    let words: Vec<String> = ["show", "sym:refill", "file:a.rs", "kind:module"]
        .map(str::to_owned)
        .into();
    assert_eq!(
        crate::source::acquisition::show_target(&words).as_deref(),
        Some("sym:refill file:a.rs kind:module")
    );
    let path_words: Vec<String> = ["show", "path:a.rs:1-2"].map(str::to_owned).into();
    assert_eq!(
        crate::source::acquisition::show_target(&path_words).as_deref(),
        Some("path:a.rs:1-2")
    );
    let (_root, _cache, store) = fixture(&[("src/b.rs", b"pub fn refill() {}\n")]);
    let error = show(&store, "sym:refill file:src/a lang:rust")
        .unwrap_err()
        .to_string();
    assert!(
        error.starts_with(
            "no_definition: no indexed definition named `refill` matching file:src/a lang:rust; 1 without filters, best src/b.rs:1-1 function"
        ),
        "{error}"
    );
}

#[test]
fn import_trails_follow_chains_stop_on_cycles_and_report_ties() {
    let (_root, _cache, store) = fixture(&[
        ("src/lib.rs", b"pub use crate::a::Alias as Top;\n"),
        ("src/a.rs", b"pub use crate::b::Thing as Alias;\n"),
        ("src/b.rs", b"pub struct Thing;\n"),
        ("src/x.rs", b"pub use crate::y::P as Q;\n"),
        ("src/y.rs", b"pub use crate::x::Q as P;\n"),
        ("src/sim/step.rs", b"use super::ledger::Ledger as L;\n"),
        ("src/sim/ledger.rs", b"pub struct Ledger;\n"),
        ("src/aa/ledger.rs", b"pub struct Ledger;\n"),
        ("src/p/thing.rs", b"pub struct Point;\n"),
        ("src/q/thing.rs", b"pub struct Point;\n"),
        ("src/user.rs", b"use thing::Point as Pt;\n"),
    ]);
    let chained = show(&store, "sym:Top").unwrap();
    assert!(chained.starts_with("src/b.rs "), "{chained}");
    assert!(
        chained.contains(
            "via: re-export of crate::a::Alias at src/lib.rs:1 -> re-export of crate::b::Thing at src/a.rs:1\n"
        ),
        "{chained}"
    );
    let cycle = show(&store, "sym:Q").unwrap();
    assert!(cycle.starts_with("src/x.rs "), "{cycle}");
    assert!(cycle.contains("(declaration not indexed)"), "{cycle}");
    let relative = show(&store, "sym:L").unwrap();
    assert!(relative.starts_with("src/sim/ledger.rs "), "{relative}");
    let tied = show(&store, "sym:Pt").unwrap();
    assert!(tied.starts_with("src/p/thing.rs "), "{tied}");
    assert!(tied.contains("at src/user.rs:1 (1 of 2)\n"), "{tied}");
    assert!(tied.contains("also: src/q/thing.rs:1-1 struct\n"), "{tied}");
}

#[test]
fn nested_test_modules_rank_after_code_and_encoded_directories_compare() {
    let (_root, _cache, store) = fixture(&[
        (
            "src/a.rs",
            b"mod inner {\n    mod tests {\n        fn build() {}\n    }\n}\n",
        ),
        ("src/zeta.rs", b"pub fn build() {}\n"),
    ]);
    assert_eq!(
        locations(&store, "sym:build", &InvocationDirectory::root())[0],
        "src/zeta.rs:function"
    );
    let spaced = InvocationDirectory::within(Path::new("/r"), Path::new("/r/my dir"));
    assert_eq!(spaced.shared_depth("my%20dir/a.rs"), 1);
}

#[test]
fn qualified_lookups_keep_exact_owners_past_broad_candidate_cap() {
    let decoys: String = "impl Tyrant { fn new() {} }\n".repeat(MAX_HITS / 2 + 1);
    let (_root, _cache, store) = fixture(&[
        ("src/a.rs", decoys.as_bytes()),
        ("src/b.rs", decoys.as_bytes()),
        ("src/z.rs", b"struct Ty;\nimpl Ty { fn new() {} }\n"),
    ]);
    let found = crate::search::definitions(
        &store,
        &Query::parse("sym:Ty::new").unwrap(),
        &InvocationDirectory::root(),
    )
    .unwrap();
    assert_eq!(found.hits.len(), 1, "{found:?}");
    assert_eq!(found.hits[0].container.as_deref(), Some("impl Ty"));
    assert!(!found.truncated);
}

#[test]
fn qualified_lookups_truncate_when_exact_owners_exceed_the_result_cap() {
    let (_root, _cache, store) = fixture(&[(
        "src/a.rs",
        b"impl Tyrant { fn new() {} }\nimpl Ty { fn new() {} }\nimpl Ty { fn new() {} }\n",
    )]);
    let paths = PathFilter::default().bind(&store.conn).unwrap();
    let found =
        qualified_hits_with_limit(&store, &QualifiedName::parse("Ty::new"), &paths, "", "", 1)
            .unwrap();
    assert_eq!(found.hits.len(), 1, "{found:?}");
    assert_eq!(found.hits[0].container.as_deref(), Some("impl Ty"));
    assert!(found.truncated);
}
