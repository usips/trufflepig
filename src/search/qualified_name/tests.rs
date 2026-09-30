use super::*;

#[test]
fn qualifier_names_impl_types_traits_and_module_paths() {
    let name = QualifiedName::parse("crate::Store::open");
    assert_eq!(name.qualifier, ["Store"]);
    assert_eq!(name.name, "open");
    let suffix = Some(QualifierMatch::Suffix);
    assert_eq!(name.matches("src/store.rs", Some("impl Store")), suffix);
    assert_eq!(
        name.matches(
            "src/a.rs",
            Some("impl<'a, T: Into<u8>> fmt::Display for Store<'a, T> where T: Copy")
        ),
        suffix
    );
    assert_eq!(name.matches("src/a.rs", Some("impl Other")), None);
    let display = QualifiedName::parse("Display::fmt");
    assert_eq!(
        display.matches("src/a.rs", Some("impl fmt::Display for Store")),
        suffix
    );
    let module = QualifiedName::parse("harvest::record");
    assert_eq!(
        module.matches("crates/lunatic-server/src/sim/harvest.rs", None),
        suffix
    );
    assert_eq!(
        module.matches("src/harvest/mod.rs", Some("impl Ledger")),
        Some(QualifierMatch::Subsequence)
    );
    assert_eq!(module.matches("src/sim/other.rs", None), None);
    assert_eq!(module.matches("src/other.rs", Some("harvest")), suffix);
    let crate_path = QualifiedName::parse("lunatic_server::sim::record");
    assert_eq!(
        crate_path.matches("crates/lunatic-server/src/sim/harvest.rs", None),
        Some(QualifierMatch::Subsequence)
    );
}

#[test]
fn relative_import_paths_resolve_against_the_importing_module() {
    let module = module_segments("crates/core/src/sim/move.rs");
    let root = crate_root_segments("crates/core/src/sim/move.rs");
    assert_eq!(root, ["crates", "core"]);
    let resolve = |text: &str| QualifiedName::parse_in(text, &module, &root).qualifier;
    assert_eq!(resolve("crate::store::Store"), ["crates", "core", "store"]);
    assert_eq!(
        resolve("self::step::Step"),
        ["crates", "core", "sim", "move", "step"]
    );
    assert_eq!(resolve("super::Ledger"), ["crates", "core", "sim"]);
    assert_eq!(resolve("super::super::Ledger"), ["crates", "core"]);
    assert_eq!(resolve("other::Thing"), ["other"]);
    assert!(crate_root_segments("lib/x.rs").is_empty());
    assert!(crate_root_segments("resrc/x.rs").is_empty());
}

#[test]
fn import_spelling_reads_aliases_and_paths() {
    assert_eq!(
        import_spelling("file_ranking::fuse_search_file_lanes as fuse_file_lanes"),
        Some((
            "file_ranking::fuse_search_file_lanes".into(),
            "fuse_file_lanes".into()
        ))
    );
    assert_eq!(
        import_spelling("store::Store"),
        Some(("store::Store".into(), "Store".into()))
    );
    assert_eq!(import_spelling("{ a, b }"), None);
}
