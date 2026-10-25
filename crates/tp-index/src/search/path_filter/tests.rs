use crate::search::{Query, search, tests::fixture};

fn paths(query: &str, files: &[(&str, &[u8])]) -> Vec<String> {
    let (_root, cache, store) = fixture(files);
    let set = search(&store, &Query::parse(query).unwrap(), false, cache.path()).unwrap();
    let mut paths: Vec<_> = set.hits.into_iter().map(|hit| hit.path).collect();
    paths.sort();
    paths.dedup();
    paths
}

const LAYOUT: &[(&str, &[u8])] = &[
    ("crates/server/src/script/host.rs", b"fn needle_host() {}\n"),
    (
        "crates/server/src/script/boundary.rs",
        b"fn needle_boundary() {}\n",
    ),
    ("crates/server/tests/script.rs", b"fn needle_test() {}\n"),
    ("web/sdk/needle.ts", b"export function needle_web() {}\n"),
    ("tools/needle.ts", b"export function needle_tool() {}\n"),
];

#[test]
fn file_values_match_root_prefixes_then_component_starts() {
    // A root-relative prefix of a file name still matches, as `boundary` of `boundary.rs`.
    assert_eq!(
        paths("re:needle file:crates/server/src/script/boundary", LAYOUT),
        ["crates/server/src/script/boundary.rs"]
    );
    // No indexed path starts with `script/host`, so it matches at a component start.
    assert_eq!(
        paths("re:needle file:script/host", LAYOUT),
        ["crates/server/src/script/host.rs"]
    );
    assert_eq!(
        paths("needle file:script/", LAYOUT),
        [
            "crates/server/src/script/boundary.rs",
            "crates/server/src/script/host.rs"
        ]
    );
    // A component match never starts mid-name.
    assert!(paths("re:needle file:ost.rs", LAYOUT).is_empty());
}

#[test]
fn root_prefix_wins_over_component_matches() {
    let files: &[(&str, &[u8])] = &[
        ("src/lib.rs", b"fn needle_root() {}\n"),
        ("crates/a/src/lib.rs", b"fn needle_member() {}\n"),
    ];
    assert_eq!(paths("sym:needle_root file:src/", files), ["src/lib.rs"]);
    assert!(paths("sym:needle_member file:src/", files).is_empty());
}

#[test]
fn root_prefix_anchors_only_at_component_or_stem_boundaries() {
    let files: &[(&str, &[u8])] = &[
        ("src-tauri/main.rs", b"fn needle_tauri() {}\n"),
        ("crates/a/src/lib.rs", b"fn needle_member() {}\n"),
    ];
    // `src` only begins `src-tauri`, so it matches at component starts.
    assert_eq!(
        paths("re:needle file:src", files),
        ["crates/a/src/lib.rs", "src-tauri/main.rs"]
    );
}

#[test]
fn file_values_are_encoded_like_indexed_paths() {
    let files: &[(&str, &[u8])] = &[
        ("caf\u{e9}/menu.rs", b"fn needle_menu() {}\n"),
        ("a b/c.rs", b"fn needle_space() {}\n"),
        ("other.rs", b"fn needle_other() {}\n"),
    ];
    assert_eq!(
        paths("re:needle file:caf\u{e9}/", files),
        ["caf%C3%A9/menu.rs"]
    );
    // Already-encoded values, as copied from results, pass through.
    assert_eq!(
        paths("re:needle file:caf%C3%A9/", files),
        ["caf%C3%A9/menu.rs"]
    );
    assert_eq!(paths("re:needle file:a%20b/", files), ["a%20b/c.rs"]);
}

#[test]
fn literal_percent_and_case_insensitive_escapes_match_indexed_paths() {
    let files: &[(&str, &[u8])] = &[
        ("100%/needle.rs", b"fn needle_percent() {}\n"),
        ("100%2f/needle.rs", b"fn needle_escape_text() {}\n"),
        ("caf\u{e9}/needle.rs", b"fn needle_unicode() {}\n"),
    ];
    assert_eq!(paths("re:needle file:100%/", files), ["100%25/needle.rs"]);
    assert_eq!(paths("re:needle file:100%25/", files), ["100%25/needle.rs"]);
    assert_eq!(
        paths("re:needle file:100%252f/", files),
        ["100%252f/needle.rs"]
    );
    assert_eq!(
        paths("re:needle file:caf%c3%a9/", files),
        ["caf%C3%A9/needle.rs"]
    );
}

#[test]
fn single_file_resolution_names_exactly_one_indexed_path() {
    let (_root, _cache, store) = fixture(&[
        (
            "crates/server/src/script/host.rs",
            b"pub struct Host;\nfn serve() {}\n",
        ),
        (
            "crates/server/src/net.rs",
            b"pub struct Net;\nfn listen() {}\n",
        ),
        ("src/lib.rs", b"fn root() {}\n"),
        ("src/lib.rs.orig", b"old\n"),
    ]);
    let resolve = |value: &str| {
        super::PathFilter::prefix(value)
            .resolve_single_file(&store.conn)
            .unwrap()
    };
    assert_eq!(resolve("./src/lib.rs").as_deref(), Some("src/lib.rs"));
    assert_eq!(
        resolve("host.rs").as_deref(),
        Some("crates/server/src/script/host.rs")
    );
    assert_eq!(
        resolve("script/host").as_deref(),
        Some("crates/server/src/script/host.rs")
    );
    assert_eq!(resolve("crates/server/src/"), None);
    // One file under a directory prefix is still a directory listing.
    assert_eq!(resolve("crates/server/src/script/"), None);
    for value in ["./crates/server/src/script/host.rs", "script/host.rs"] {
        let set = crate::search::map(&store, value).unwrap();
        assert!(
            set.hits.iter().any(|hit| hit.name == "serve"),
            "{value}: {:?}",
            set.hits
        );
    }
}

#[test]
fn repeated_file_values_or_and_negated_values_exclude() {
    assert_eq!(
        paths("re:needle file:web/ file:tools/", LAYOUT),
        ["tools/needle.ts", "web/sdk/needle.ts"]
    );
    assert_eq!(
        paths("needle file:web/ file:tools/", LAYOUT),
        ["tools/needle.ts", "web/sdk/needle.ts"]
    );
    assert_eq!(
        paths("re:needle file:crates/ -file:tests", LAYOUT),
        [
            "crates/server/src/script/boundary.rs",
            "crates/server/src/script/host.rs"
        ]
    );
    assert_eq!(
        paths("needle -file:crates/ -file:tools/", LAYOUT),
        ["web/sdk/needle.ts"]
    );
    // `-file:` is a filter, never query text.
    let query = Query::parse("needle -file:tests").unwrap();
    assert_eq!(query.text, "needle");
    assert_eq!(query.path.excludes(), ["tests"]);
}

#[test]
fn map_prefix_uses_the_same_path_filter() {
    let (_root, _cache, store) = fixture(&[
        ("crates/server/src/script/host.rs", b"pub struct Host;\n"),
        ("crates/server/src/net.rs", b"pub struct Net;\n"),
    ]);
    let set = crate::search::map(&store, "script/").unwrap();
    assert!(!set.hits.is_empty());
    assert!(
        set.hits
            .iter()
            .all(|hit| hit.path == "crates/server/src/script/host.rs")
    );
}

#[test]
fn sql_predicate_agrees_with_rust_matcher() {
    let (_root, _cache, store) = fixture(LAYOUT);
    for query in [
        "file:script/host",
        "file:crates/ -file:tests",
        "file:web/ file:tools/",
        "-file:needle.ts",
        "file:ost.rs",
        "file:crates/server/src/script/boundary",
        "file:it's/",
        "",
    ] {
        let query = Query::parse(query).unwrap();
        let bound = query.path.bind(&store.conn).unwrap();
        let mut statement = store
            .conn
            .prepare(&format!(
                "SELECT path FROM files f WHERE 1{} ORDER BY path",
                bound.sql_clause("f.path")
            ))
            .unwrap();
        let from_sql: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let from_rust: Vec<_> = LAYOUT
            .iter()
            .map(|(path, _)| path.to_string())
            .filter(|path| bound.matches(path))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        assert_eq!(from_sql, from_rust, "{query:?}");
    }
}

#[test]
fn empty_filtered_results_explain_which_filter_matched_nothing() {
    let (_root, cache, store) = fixture(LAYOUT);
    let diagnosis = |text: &str| {
        let set = search(&store, &Query::parse(text).unwrap(), false, cache.path()).unwrap();
        assert!(set.hits.is_empty(), "{text}");
        set.coverage["filter_diagnosis"].as_str().map(str::to_owned)
    };
    assert_eq!(
        diagnosis("re:needle file:script/hosts").unwrap(),
        "file:script/hosts matched 0 indexed paths (nearest: crates/server/src/script/host.rs)"
    );
    assert_eq!(
        diagnosis("re:needle file:crates/sever/testz/").unwrap(),
        "file:crates/sever/testz/ matched 0 indexed paths (nearest: crates/server/tests/)"
    );
    // Typos past the shared prefix rank by edit distance.
    assert_eq!(
        diagnosis("re:needle file:crates/server/scripz/").unwrap(),
        "file:crates/server/scripz/ matched 0 indexed paths (nearest: crates/server/tests/script.rs, crates/server/src/script/)"
    );
    assert_eq!(
        diagnosis("re:absent file:script/").unwrap(),
        "file:script/ matched 2 indexed paths"
    );
    assert_eq!(
        diagnosis("needle lang:php").unwrap(),
        "lang:php matched 0 files"
    );
    assert_eq!(
        diagnosis("needle file:web/ lang:rust").unwrap(),
        "file:web/ matched 1 indexed path; filters matched 0 files together"
    );
    assert_eq!(
        diagnosis("needle file:web/ -file:sdk").unwrap(),
        "file:web/ matched 1 indexed path; -file: excluded all 1 matching files"
    );
    assert_eq!(diagnosis("zzqq"), None);
}

#[test]
fn unknown_languages_fail_and_aliases_resolve() {
    let error = Query::parse("needle lang:python").unwrap_err().to_string();
    assert!(error.starts_with("unknown_language: python; known: rust,"));
    assert_eq!(Query::parse("x lang:markdown").unwrap().language, "text");
    assert_eq!(Query::parse("x lang:RS").unwrap().language, "rust");
    assert_eq!(Query::parse("x lang:").unwrap().language, "");
}
