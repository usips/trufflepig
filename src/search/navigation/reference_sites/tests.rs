use super::*;
use crate::search::tests::fixture;

fn sites(store: &Store, text: &str) -> Vec<String> {
    references(store, &reference_query(text).unwrap())
        .unwrap()
        .hits
        .into_iter()
        .map(|hit| {
            format!(
                "{}:{} {} {}",
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
        ("tests/it.rs", b"fn t() { alpha(); }\n"),
    ]);
    let found = sites(&store, "alpha");
    assert_eq!(found[0], "src/z.rs:1 declaration resolved", "{found:?}");
    assert!(
        found[1].starts_with("src/b.rs:3 call") && found[1].ends_with(" ×2"),
        "{found:?}"
    );
    assert!(found[2].starts_with("src/b.rs:1 declaration"), "{found:?}");
    assert_eq!(
        found[3..]
            .iter()
            .map(|site| site.split(' ').next().unwrap())
            .collect::<Vec<_>>(),
        ["src/z.rs:4", "tests/it.rs:1"],
        "{found:?}"
    );
    let set = references(&store, &reference_query("alpha").unwrap()).unwrap();
    assert_eq!(set.coverage["reference_sites"], 6);
    assert_eq!(set.coverage["reference_files"], 3);
    assert_eq!(
        crate::results::lines::single_repo_coverage(&set.coverage),
        "indexed 3/3; refs 6 sites in 3 files"
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
