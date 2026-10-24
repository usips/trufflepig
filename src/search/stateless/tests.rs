mod filename_scan;
mod scoped_lookup;

use super::scan::MAX_CURRENT_FILE_BYTES;
use super::*;
use crate::{
    daemon::deadline::QueryDeadline,
    output::OutputBudget,
    search::{InvocationDirectory, Query},
    source,
    store::Store,
};
use anyhow::Result;
use serde_json::Value;
use std::{fs, path::Path, time::Duration};

fn deadline() -> QueryDeadline {
    QueryDeadline::after(Duration::from_secs(5))
}

fn create_root(directory: &Path) -> Result<(std::path::PathBuf, std::path::PathBuf)> {
    let root = directory.join("repo");
    fs::create_dir_all(&root)?;
    Ok((root, directory.join("never-created-cache")))
}

#[test]
fn cache_ancestor_refuses_lookup_instead_of_reporting_exhaustive_absence() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (root, _) = create_root(directory.path())?;
    fs::write(root.join("current.rs"), "pub fn present_needle() {}\n")?;
    for cache in [root.as_path(), directory.path()] {
        for text in [
            "present_needle",
            "sym:present_needle",
            "re:present_needle",
            "file:current.rs kind:file",
        ] {
            let error = search(
                &root,
                cache,
                &Query::parse(text)?,
                &InvocationDirectory::root(),
                &deadline(),
                false,
                false,
            )
            .unwrap_err();
            assert!(error.to_string().starts_with("invalid_cache:"), "{error:#}");
        }
        let error = map_file(&root, cache, "current.rs", &deadline()).unwrap_err();
        assert!(error.to_string().starts_with("invalid_cache:"), "{error:#}");
    }
    assert_eq!(fs::read_dir(&root)?.count(), 1);
    Ok(())
}

#[test]
fn current_search_matches_exact_file_filter_and_uses_cache_free_handles() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (root, cache) = create_root(directory.path())?;
    fs::create_dir_all(root.join("src"))?;
    fs::create_dir_all(root.join("tests"))?;
    fs::write(
        root.join("src/current.rs"),
        "pub fn refreshed_lookup() {}\n",
    )?;
    fs::write(
        root.join("tests/other.rs"),
        "pub fn refreshed_lookup() {}\n",
    )?;

    let query = Query::parse("sym:refreshed_lookup file:src/")?;
    let result = search(
        &root,
        &cache,
        &query,
        &InvocationDirectory::root(),
        &deadline(),
        false,
        false,
    )?;
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].path, "src/current.rs");
    assert!(result.hits[0].handle.starts_with("read:path:"));
    assert!(!cache.exists());

    let expected_cache = directory.path().join("indexed-cache");
    let mut indexed = Store::open(&root, &expected_cache)?;
    indexed.index()?;
    let mut trace = crate::search::telemetry::RetrievalTrace::disabled();
    let expected = crate::search::search_prepared(
        &indexed,
        &query,
        &expected_cache,
        None,
        None,
        &mut trace,
        &deadline(),
    )?;
    let current_rows: Vec<_> = result
        .hits
        .iter()
        .map(|hit| (&hit.path, hit.start, hit.end, &hit.name, &hit.kind))
        .collect();
    let indexed_rows: Vec<_> = expected
        .hits
        .iter()
        .map(|hit| (&hit.path, hit.start, hit.end, &hit.name, &hit.kind))
        .collect();
    assert_eq!(current_rows, indexed_rows);

    let store = Store::unpublished(&root, &cache)?;
    let shown = source::show(&store, &result.hits[0].handle, &OutputBudget::new(1_000)?)?;
    let page: Value = serde_json::from_str(&shown)?;
    assert_eq!(page["lines"][0]["text"], "pub fn refreshed_lookup() {}\n");
    assert_eq!(page["verified"], false);
    assert!(!cache.exists());

    fs::write(root.join("src/current.rs"), "pub fn replaced_lookup() {}\n")?;
    let changed = Store::unpublished(&root, &cache)?;
    let error =
        source::show(&changed, &result.hits[0].handle, &OutputBudget::new(1_000)?).unwrap_err();
    assert!(error.to_string().starts_with("stale_source:"));
    assert!(!cache.exists());
    Ok(())
}

#[test]
fn current_map_outlines_only_the_requested_current_file() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (root, cache) = create_root(directory.path())?;
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("src/map.rs"),
        "pub struct CurrentMap;\npub fn current_outline() {}\n",
    )?;

    let first = map_file(&root, &cache, "src/map.rs", &deadline())?;
    let names: Vec<_> = first.hits.iter().map(|hit| hit.name.as_str()).collect();
    assert!(names.contains(&"CurrentMap"));
    assert!(names.contains(&"current_outline"));
    assert!(
        first
            .hits
            .iter()
            .all(|hit| hit.handle.starts_with("read:path:"))
    );
    assert!(!cache.exists());

    fs::write(root.join("src/map.rs"), "pub fn changed_outline() {}\n")?;
    let second = map_file(&root, &cache, "src/map.rs", &deadline())?;
    assert!(second.hits.iter().any(|hit| hit.name == "changed_outline"));
    assert!(!second.hits.iter().any(|hit| hit.name == "current_outline"));
    assert!(!cache.exists());
    Ok(())
}

#[test]
fn excluded_current_bytes_do_not_claim_exhaustive_absence() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (root, cache) = create_root(directory.path())?;
    fs::write(
        root.join("large.txt"),
        vec![b'x'; MAX_CURRENT_FILE_BYTES + 1],
    )?;

    let query = Query::parse("never_present")?;
    let result = search(
        &root,
        &cache,
        &query,
        &InvocationDirectory::root(),
        &deadline(),
        false,
        false,
    )?;
    assert!(result.hits.is_empty());
    assert_eq!(result.coverage["absence"], "not_proven");
    assert_eq!(result.coverage["current_source"]["excluded_files"], 1);
    assert!(result.truncated);
    assert!(!cache.exists());
    Ok(())
}

#[test]
fn requested_semantic_lanes_are_reported_unavailable() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (root, cache) = create_root(directory.path())?;
    fs::write(root.join("source.txt"), "Current source lookup evidence.\n")?;

    let query = Query::parse("Current source lookup")?;
    let result = search(
        &root,
        &cache,
        &query,
        &InvocationDirectory::root(),
        &deadline(),
        true,
        true,
    )?;
    assert_eq!(result.coverage["semantic_status"], "unavailable");
    assert_eq!(result.coverage["rerank_status"], "unavailable");
    assert!(!cache.exists());
    Ok(())
}

#[test]
fn regex_kind_filter_reports_unmeasured_parser_coverage() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (root, cache) = create_root(directory.path())?;
    fs::write(root.join("source.rs"), "pub fn current_function() {}\n")?;

    let query = Query::parse("re:absent_pattern kind:function")?;
    let result = search(
        &root,
        &cache,
        &query,
        &InvocationDirectory::root(),
        &deadline(),
        false,
        false,
    )?;
    assert!(result.hits.is_empty());
    assert_eq!(result.coverage["absence"], "not_proven");
    assert_eq!(
        result.coverage["current_source"]["unsearched_unknown"],
        true
    );
    assert!(result.truncated);
    assert!(!cache.exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn current_map_rejects_traversal_and_symlink_escape() -> Result<()> {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir()?;
    let (root, cache) = create_root(directory.path())?;
    let outside = directory.path().join("outside.rs");
    fs::write(&outside, "pub fn outside() {}\n")?;
    symlink(&outside, root.join("alias.rs"))?;

    assert!(map_file(&root, &cache, "../outside.rs", &deadline()).is_err());
    assert!(map_file(&root, &cache, "alias.rs", &deadline()).is_err());
    Ok(())
}

#[test]
fn map_of_a_directory_requires_one_current_file() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (root, cache) = create_root(directory.path())?;
    fs::create_dir_all(root.join("src"))?;
    let error = map_file(&root, &cache, "src", &deadline()).unwrap_err();
    assert!(
        crate::coded_error::ErrorCode::StatelessMapRequiresFile.is_code_of(&error),
        "{error:#}"
    );
    Ok(())
}
