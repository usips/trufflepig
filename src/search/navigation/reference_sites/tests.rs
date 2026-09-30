use super::*;
use crate::search::tests::fixture;

fn sites(store: &Store, text: &str) -> Vec<String> {
    references(store, &reference_query(text).unwrap())
        .unwrap()
        .hits
        .into_iter()
        .map(|hit| {
            let repeats = hit.repeats.map(|n| format!(" x{n}")).unwrap_or_default();
            format!(
                "{}:{} {} {}{repeats}",
                hit.path,
                hit.start_line,
                hit.kind,
                hit.resolution.unwrap_or_default()
            )
        })
        .collect()
}

#[test]
fn refs_lead_with_the_declaration_then_code_imports_and_tests() {
    let (_root, _cache, store) = fixture(&[
        (
            "src/b.rs",
            b"use crate::z::alpha;\nfn b() {\n    alpha(); alpha();\n}\n",
        ),
        (
            "src/z.rs",
            b"pub fn alpha() {}\n#[cfg(test)]\nmod tests {\n    fn t() { super::alpha(); }\n}\n",
        ),
        ("tests/it.rs", b"fn alpha() {}\nfn t() { alpha(); }\n"),
    ]);
    let found = sites(&store, "alpha");
    assert_eq!(found[0], "src/z.rs:1 declaration resolved", "{found:?}");
    assert!(found[1] == "src/b.rs:3 call resolved x2", "{found:?}");
    assert!(found[2].starts_with("src/b.rs:1 declaration"), "{found:?}");
    assert_eq!(
        found[3..]
            .iter()
            .map(|site| site.split(' ').next().unwrap())
            .collect::<Vec<_>>(),
        ["src/z.rs:4", "tests/it.rs:1", "tests/it.rs:2"],
        "{found:?}"
    );
    let set = references(&store, &reference_query("alpha").unwrap()).unwrap();
    assert_eq!(set.coverage["reference_sites"], 7);
    assert_eq!(set.coverage["reference_files"], 3);
    assert_eq!(
        crate::results::lines::single_repo_coverage(&set.coverage),
        "indexed 3/3; refs 7 sites in 3 files"
    );
}

#[test]
fn refs_honor_path_language_and_kind_words() {
    let (_root, _cache, store) = fixture(&[
        ("src/b.rs", b"fn b() { alpha(); }\n"),
        ("src/z.rs", b"pub fn alpha() {}\n"),
        ("web/a.ts", b"function alpha() {}\nalpha();\n"),
    ]);
    let paths = |text: &str| -> Vec<String> {
        sites(&store, text)
            .into_iter()
            .map(|site| site.split(' ').next().unwrap().to_owned())
            .collect()
    };
    assert_eq!(paths("alpha file:src/b"), ["src/b.rs:1"]);
    assert_eq!(paths("alpha lang:typescript"), ["web/a.ts:1", "web/a.ts:2"]);
    assert_eq!(
        paths("alpha kind:declaration"),
        ["src/z.rs:1", "web/a.ts:1"]
    );
    assert_eq!(paths("refs:alpha lang:rust kind:call"), ["src/b.rs:1"]);
}

#[test]
fn qualified_refs_keep_sites_owned_by_the_named_type() {
    let (_root, _cache, store) = fixture(&[(
        "src/a.rs",
        b"struct A;\nimpl A {\n    fn go() {}\n}\nstruct B;\nimpl B {\n    fn go() {}\n}\n",
    )]);
    assert_eq!(sites(&store, "A::go"), ["src/a.rs:3 declaration resolved"]);
}

#[test]
fn same_line_sites_collapse_only_with_equal_role_and_resolution() {
    let (_root, _cache, store) = fixture(&[(
        "src/a.rs",
        b"fn go() { go(); }\nfn twice() { other::go(); other::go(); }\n",
    )]);
    let found = sites(&store, "go");
    assert_eq!(found[0], "src/a.rs:1 declaration resolved", "{found:?}");
    assert!(found[1].starts_with("src/a.rs:1 call "), "{found:?}");
    assert!(
        found[2].starts_with("src/a.rs:2 call ") && found[2].ends_with(" x2"),
        "{found:?}"
    );
    assert_eq!(found.len(), 3, "{found:?}");
    let set = references(&store, &reference_query("go").unwrap()).unwrap();
    let page = crate::results::compact_hit(
        &set.hits[2],
        std::path::Path::new("/r"),
        None,
        crate::results::HitDetail {
            names: true,
            snippets: false,
        },
    )
    .unwrap();
    assert_eq!(page["repeats"], 2);
    assert!(!page["resolution"].as_str().unwrap().contains('×'));
    let line = crate::results::lines::entry_line(
        &crate::results::ResultEntry::LiveSource(set.hits[2].clone()),
        None,
        crate::results::HitDetail {
            names: true,
            snippets: false,
        },
    );
    assert!(line.ends_with("\t×2"), "{line}");
}
