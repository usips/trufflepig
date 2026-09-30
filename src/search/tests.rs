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
fn php_addon_symbols_support_search_map_and_show() {
    let php = br#"<?php
namespace XenForo\AddOn\Kiwifarms\Job;

class RefreshDelegation
{
    public function run(): bool
    {
        return $this->refreshQueuedJobs();
    }

    private function refreshQueuedJobs(): bool
    {
        return true;
    }
}
"#;
    let (_root, cache, store) = fixture(&[("src/Job/RefreshDelegation.php", php)]);

    let class = search(
        &store,
        &Query::parse("sym:RefreshDelegation lang:php").unwrap(),
        false,
        cache.path(),
    )
    .unwrap();
    assert_eq!(class.hits.len(), 1, "{:?}", class.hits);
    assert_eq!(class.hits[0].path, "src/Job/RefreshDelegation.php");
    assert_eq!(class.hits[0].kind, "class");

    let method = search(
        &store,
        &Query::parse("sym:refreshQueuedJobs lang:phtml").unwrap(),
        false,
        cache.path(),
    )
    .unwrap();
    assert_eq!(method.hits.len(), 1, "{:?}", method.hits);
    assert_eq!(method.hits[0].kind, "method");

    let outline = map(&store, "src/Job/RefreshDelegation.php").unwrap();
    for name in ["RefreshDelegation", "run", "refreshQueuedJobs"] {
        assert!(
            outline.hits.iter().any(|hit| hit.name == name),
            "{name}: {:?}",
            outline.hits
        );
    }

    let shown = source::show(
        &store,
        "sym:RefreshDelegation lang:php",
        &OutputBudget::new(4000).unwrap(),
    )
    .unwrap();
    assert!(shown.contains("class RefreshDelegation"), "{shown}");
}

#[test]
fn csharp_and_javascript_index_search_and_navigation_contracts() {
    let (_root, cache, store) = fixture(&[
        (
            "wallet.js",
            b"function Wallet() { return null; }\nfunction refreshWallet(id) { return findWallet(id); }\nfunction findWallet(id) { return id; }\nwindow.copyToClipboard = async function (e, data) { return data; };\n",
        ),
        (
            "Wallet.cs",
            b"namespace Shop;\npublic record Wallet(string Id);\npublic interface IWalletStore { Wallet Load(); }\npublic sealed class WalletStore : IWalletStore { public Wallet Load() { return new Wallet(\"x\"); } }\npublic sealed class WalletFactory { public event System.Action Changed; public WalletFactory() {} }\n",
        ),
    ]);

    for alias in ["js", "javascript"] {
        let set = search(
            &store,
            &Query::parse(&format!("sym:Wallet lang:{alias}")).unwrap(),
            false,
            cache.path(),
        )
        .unwrap();
        assert_eq!(set.hits.len(), 1, "lang:{alias}: {:?}", set.hits);
        assert_eq!(set.hits[0].path, "wallet.js");
        assert_eq!(set.hits[0].kind, "function");
    }

    for alias in ["cs", "c#", "csharp"] {
        let set = search(
            &store,
            &Query::parse(&format!("sym:Wallet lang:{alias}")).unwrap(),
            false,
            cache.path(),
        )
        .unwrap();
        assert!(
            !set.hits.is_empty(),
            "lang:{alias} returned no Wallet definitions"
        );
        assert!(set.hits.iter().all(|hit| hit.path == "Wallet.cs"));
        assert_eq!(set.hits[0].kind, "record", "lang:{alias}: {:?}", set.hits);
    }

    let csharp_map = map(&store, "Wallet.cs").unwrap();
    for (name, kind) in [
        ("Shop", "namespace"),
        ("Wallet", "record"),
        ("IWalletStore", "interface"),
        ("WalletStore", "class"),
        ("WalletFactory", "class"),
        ("Load", "method"),
        ("Changed", "event"),
    ] {
        assert!(
            csharp_map
                .hits
                .iter()
                .any(|hit| hit.name == name && hit.kind == kind),
            "missing {kind} {name} from map: {:?}",
            csharp_map.hits
        );
    }
    let js_map = map(&store, "wallet.js").unwrap();
    assert!(js_map.hits.iter().any(|hit| hit.name == "refreshWallet"));
    assert!(js_map.hits.iter().any(|hit| hit.name == "copyToClipboard"));

    let budget = OutputBudget::new(4000).unwrap();
    let shown = source::show(&store, "sym:Wallet lang:c#", &budget).unwrap();
    assert!(shown.contains("public record Wallet"), "{shown}");
    let factory = search(
        &store,
        &Query::parse("sym:WalletFactory lang:c#").unwrap(),
        false,
        cache.path(),
    )
    .unwrap();
    assert_eq!(factory.hits[0].kind, "class", "{:?}", factory.hits);
    assert!(factory.hits.iter().any(|hit| hit.kind == "constructor"));
    let shown = source::show(&store, "sym:copyToClipboard lang:js", &budget).unwrap();
    assert!(
        shown.contains("window.copyToClipboard = async function"),
        "{shown}"
    );
    let shown = source::show(&store, "sym:WalletFactory lang:c#", &budget).unwrap();
    assert!(
        shown.contains("public sealed class WalletFactory"),
        "{shown}"
    );

    let js_references = references(&store, &reference_query("findWallet").unwrap()).unwrap();
    assert!(
        js_references
            .hits
            .iter()
            .any(|hit| { hit.path == "wallet.js" && hit.kind == "call" && hit.target.is_some() })
    );
    let csharp_references = references(&store, &reference_query("Wallet").unwrap()).unwrap();
    assert!(
        csharp_references
            .hits
            .iter()
            .any(|hit| hit.path == "Wallet.cs")
    );
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
    let set = references(&store, &reference_query("alpha").unwrap()).unwrap();
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
fn exact_search_lists_declarations_before_imports_of_the_name() {
    let (_root, cache, store) = fixture(&[
        ("a.rs", b"use crate::z::Coord;\nfn f(_: Coord) {}\n"),
        ("z.rs", b"pub struct Coord;\n"),
    ]);
    let set = search(
        &store,
        &Query::parse("sym:Coord").unwrap(),
        false,
        cache.path(),
    )
    .unwrap();
    assert_eq!(set.hits[0].path, "z.rs", "{:?}", set.hits);
    assert_eq!(set.hits[0].kind, "struct");
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
fn map_of_one_file_lists_its_functions_and_a_prefix_lists_one_row_per_file() {
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
    assert_eq!(names("src/lib.rs"), ["Bucket", "refill"]);
    assert_eq!(names("src/"), ["Bucket, refill"]);
}
