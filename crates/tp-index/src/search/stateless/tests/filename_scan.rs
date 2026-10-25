use super::{create_root, deadline};
use crate::{
    search::{InvocationDirectory, Query},
    source,
    store::Store,
};
use anyhow::Result;
use serde_json::Value;
use std::fs;
use tp_core::output::OutputBudget;

#[test]
fn filter_only_file_lookup_reaches_past_catalog_cap_without_parsing_sources() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (root, cache) = create_root(directory.path())?;
    let unrelated = root.join("a");
    fs::create_dir_all(&unrelated)?;
    for index in 0..4_097 {
        fs::write(
            unrelated.join(format!("{index:04}.rs")),
            "pub fn unrelated_source() {}\n",
        )?;
    }
    let target = root.join("a05_baselines.rs");
    let target_source = "pub fn current_baseline() {}\n";
    fs::write(&target, target_source)?;

    let result = super::super::search(
        &root,
        &cache,
        &Query::parse("kind:file file:a05_baselines.rs")?,
        &InvocationDirectory::root(),
        &deadline(),
        false,
        false,
    )?;

    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].path, "a05_baselines.rs");
    assert_eq!(result.hits[0].kind, "file");
    assert!(result.hits[0].handle.starts_with("read:path:"));
    assert_eq!(result.coverage["current_source"]["complete"], true);
    assert_eq!(result.coverage["current_source"]["read_files"], 1);
    assert_eq!(
        result.coverage["current_source"]["bytes_read"],
        target_source.len()
    );
    assert_eq!(result.coverage["current_source"]["facts_indexed"], 0);
    assert_eq!(result.coverage["current_source"]["parse_failures"], 0);
    assert!(!cache.exists());

    let store = Store::unpublished(&root, &cache)?;
    let shown = source::show(&store, &result.hits[0].handle, &OutputBudget::new(1_000)?)?;
    let page: Value = serde_json::from_str(&shown)?;
    assert_eq!(page["lines"][0]["text"], target_source);

    fs::write(&target, "pub fn changed_baseline() {}\n")?;
    let changed = Store::unpublished(&root, &cache)?;
    let error =
        source::show(&changed, &result.hits[0].handle, &OutputBudget::new(1_000)?).unwrap_err();
    assert!(error.to_string().starts_with("stale_source:"));
    assert!(!cache.exists());
    Ok(())
}

#[test]
fn file_kind_text_query_finds_content_without_a_matching_filename() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (root, cache) = create_root(directory.path())?;
    let text = "pub fn process() { let _ = \"zzbodyonlymarker\"; }\n";
    fs::write(root.join("unrelated.rs"), text)?;
    let result = super::super::search(
        &root,
        &cache,
        &Query::parse("zzbodyonlymarker kind:file")?,
        &InvocationDirectory::root(),
        &deadline(),
        false,
        false,
    )?;
    let hit = result.hits.iter().find(|hit| hit.path == "unrelated.rs");
    let hit = hit.expect("free-text file queries search body content");
    assert_eq!(hit.kind, "file");
    assert!(hit.handle.starts_with("read:path:"));
    let store = Store::unpublished(&root, &cache)?;
    let shown = source::show(&store, &hit.handle, &OutputBudget::new(1_000)?)?;
    let page: Value = serde_json::from_str(&shown)?;
    assert_eq!(page["lines"][0]["text"], text);
    assert_eq!(result.coverage["current_source"]["read_files"], 1);
    assert_eq!(result.coverage["current_source"]["bytes_read"], text.len());
    assert!(!cache.exists());
    Ok(())
}
