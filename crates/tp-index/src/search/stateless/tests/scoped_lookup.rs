use super::*;

#[test]
fn scoped_symbol_lookup_parses_only_selected_files_after_a_large_catalog() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (root, cache) = create_root(directory.path())?;
    fs::create_dir_all(root.join("unrelated"))?;
    for index in 0..4_097 {
        fs::write(root.join(format!("unrelated/file_{index}.rs")), [0u8])?;
    }
    fs::create_dir_all(root.join("src/spec"))?;
    fs::write(
        root.join("src/spec/world_harness.rs"),
        "pub fn tick_hz() -> u32 { 60 }\n",
    )?;
    let result = search(
        &root,
        &cache,
        &Query::parse("sym:tick_hz file:src/spec/")?,
        &InvocationDirectory::root(),
        &deadline(),
        options(),
    )?;
    assert!(
        result
            .hits
            .iter()
            .any(|hit| { hit.path == "src/spec/world_harness.rs" && hit.name == "tick_hz" })
    );
    let current = &result.coverage["current_source"];
    assert_eq!(current["read_files"], 1);
    assert_eq!(current["excluded_files"], 0);
    assert_eq!(current["complete"], true);
    assert_eq!(current["path_catalog_truncated"], false);
    assert!(!cache.exists());

    let absent = search(
        &root,
        &cache,
        &Query::parse("sym:absent file:src/spec/")?,
        &InvocationDirectory::root(),
        &deadline(),
        options(),
    )?;
    assert!(absent.hits.is_empty());
    assert_eq!(absent.coverage["absence"], "exhaustive");
    assert!(!absent.truncated);
    Ok(())
}
