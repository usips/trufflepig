use super::*;
use tp_core::coded_error::ErrorCode;

#[test]
fn cache_free_symbol_show_follows_filtered_import_and_keeps_a_current_cursor() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (root, cache) = create_root(directory.path())?;
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("src/lib.rs"),
        "pub mod model;\npub use crate::model::ReadonlySymbol as Alias;\n",
    )?;
    let fields = (0..230)
        .map(|index| format!("    pub value_{index}: usize,\n"))
        .collect::<String>();
    fs::write(
        root.join("src/model.rs"),
        format!("pub struct ReadonlySymbol {{\n{fields}}}\n"),
    )?;

    let read = crate::search::stateless::show_symbol(
        &root,
        &cache,
        "sym:Alias file:src/lib.rs",
        &InvocationDirectory::root(),
        &deadline(),
    )?;
    assert_eq!(read.source.path, "src/model.rs");
    assert!(read.source.handle.starts_with("path:"));
    assert!(read.coverage["complete"] == true);
    let selection = read.source.definitions.as_ref().expect("symbol selection");
    assert_eq!(selection.imports, 1);
    assert_eq!(selection.declarations, 0);
    assert!(
        selection
            .import
            .as_ref()
            .is_some_and(|import| import.followed)
    );
    assert!(!cache.exists());

    let rendered = source::render_owned(
        read.source,
        &OutputBudget::new(4_000)?,
        &serde_json::json!({"coverage":read.coverage}),
    )?;
    let page: Value = serde_json::from_str(&rendered)?;
    let continuation = page["next"].as_str().expect("large source continuation");
    assert!(continuation.starts_with("read:path:"));

    let store = Store::unpublished(&root, &cache)?;
    let continued = source::show(&store, continuation, &OutputBudget::new(4_000)?)?;
    let continued: Value = serde_json::from_str(&continued)?;
    assert_eq!(continued["verified"], false);
    assert!(continued["lines"].to_string().contains("value_220"));
    assert!(!cache.exists());

    fs::write(root.join("src/model.rs"), "pub struct Replacement;\n")?;
    let changed = Store::unpublished(&root, &cache)?;
    let error = source::show(&changed, continuation, &OutputBudget::new(4_000)?).unwrap_err();
    assert!(ErrorCode::StaleSource.is_code_of(&error), "{error:#}");
    assert!(!cache.exists());
    Ok(())
}

#[test]
fn incomplete_symbol_snapshot_does_not_claim_no_definition() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (root, cache) = create_root(directory.path())?;
    fs::write(root.join("unreadable.rs"), b"MissingSymbol\0")?;

    let error = crate::search::stateless::show_symbol(
        &root,
        &cache,
        "sym:MissingSymbol",
        &InvocationDirectory::root(),
        &deadline(),
    )
    .err()
    .expect("an incomplete snapshot cannot select a definition");
    assert!(ErrorCode::SourceUnavailable.is_code_of(&error), "{error:#}");
    assert!(
        error.to_string().contains("could not rank a complete"),
        "{error:#}"
    );
    assert!(
        error.to_string().contains("\"complete\":false"),
        "{error:#}"
    );
    assert!(!cache.exists());
    Ok(())
}

#[test]
fn cache_free_symbol_show_prefilters_large_unrelated_catalogs() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (root, cache) = create_root(directory.path())?;
    for index in 0..1_100 {
        fs::write(
            root.join(format!("unrelated_{index}.rs")),
            b"unrelated text\n",
        )?;
    }
    fs::write(root.join("symbol.rs"), "pub struct CatalogSymbol;\n")?;

    let read = crate::search::stateless::show_symbol(
        &root,
        &cache,
        "sym:CatalogSymbol",
        &InvocationDirectory::root(),
        &deadline(),
    )?;
    assert_eq!(read.source.path, "symbol.rs");
    assert_eq!(read.coverage["current_source"]["walked_files"], 1_101);
    assert_eq!(read.coverage["current_source"]["read_files"], 1);
    assert_eq!(read.coverage["complete"], true);
    assert!(!cache.exists());
    Ok(())
}

#[test]
fn cache_free_symbol_show_matches_indexed_ranking_and_filters() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (root, cache) = create_root(directory.path())?;
    for path in ["src/feature", "tests"] {
        fs::create_dir_all(root.join(path))?;
    }
    for path in ["src/lib.rs", "src/feature/local.rs", "tests/integration.rs"] {
        fs::write(root.join(path), "pub struct RankedSymbol;\n")?;
    }
    fs::write(
        root.join("src/feature/local.ts"),
        "export class RankedSymbol {}\n",
    )?;
    let mut indexed = Store::open(&root, &directory.path().join("indexed-cache"))?;
    indexed.index()?;

    for invocation in [&root, &root.join("src/feature")] {
        let origin = InvocationDirectory::within(&root, invocation);
        for target in [
            "sym:RankedSymbol",
            "sym:RankedSymbol kind:struct",
            "sym:RankedSymbol lang:ts",
            "sym:RankedSymbol file:src/ -file:feature",
        ] {
            let current =
                crate::search::stateless::show_symbol(&root, &cache, target, &origin, &deadline())?;
            let expected = source::acquisition::acquire(&indexed, target, None, &origin)?;
            assert_eq!(
                current.source.path, expected.path,
                "{target} from {invocation:?}"
            );
            let budget = OutputBudget::new(4_000)?;
            let current: Value = serde_json::from_str(&source::render_owned(
                current.source,
                &budget,
                &serde_json::json!({}),
            )?)?;
            let expected: Value = serde_json::from_str(&source::render_owned(
                expected,
                &budget,
                &serde_json::json!({}),
            )?)?;
            let text = |page: &Value| {
                page["lines"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|line| line["text"].as_str().unwrap())
                    .collect::<String>()
            };
            // Stateless path cursors pad the selected body to complete source lines.
            assert!(text(&current).contains(text(&expected).trim_end_matches('\n')));
            for key in ["definitions", "imports", "also"] {
                assert_eq!(
                    current[key], expected[key],
                    "{key}: {target} from {invocation:?}"
                );
            }
        }
    }
    assert!(!cache.exists());
    Ok(())
}
