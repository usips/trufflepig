use super::*;
use crate::search::stateless::{read_cutoff::ReadCutoff, stateless_scan};

fn write_many(root: &Path, directory: &str, count: usize, body: &str) -> Result<()> {
    fs::create_dir_all(root.join(directory))?;
    for index in 0..count {
        fs::write(root.join(format!("{directory}/f{index:05}.rs")), body)?;
    }
    Ok(())
}

#[test]
fn deadline_after_some_reads_returns_hits_found_so_far_with_a_follow_up() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (root, cache) = create_root(directory.path())?;
    write_many(&root, "src/deep", 10, "pub fn shared_needle() {}\n")?;
    let query = Query::parse("sym:shared_needle")?;
    let deadline = deadline();
    let result = stateless_scan::search_root(
        &root,
        &cache,
        &query,
        &InvocationDirectory::root(),
        &deadline,
        options(),
        ReadCutoff::after_reads(&deadline, 3),
    )?;
    let paths: Vec<_> = result.hits.iter().map(|hit| hit.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "src/deep/f00000.rs",
            "src/deep/f00001.rs",
            "src/deep/f00002.rs"
        ]
    );
    assert!(
        result
            .hits
            .iter()
            .all(|hit| hit.handle.starts_with("read:path:"))
    );
    let current = &result.coverage["current_source"];
    assert_eq!(current["complete"], false);
    assert_eq!(current["limit"], "deadline");
    assert_eq!(current["read_files"], 3);
    assert_eq!(current["unsearched_files"], 7);
    assert_eq!(result.coverage["complete"], false);
    assert!(result.truncated);
    let hint = result.coverage["hint"].as_str().unwrap();
    assert!(
        hint.starts_with("deadline: searched 3 files, 7 unsearched"),
        "{hint}"
    );
    assert_eq!(
        result.coverage["next_step"],
        "search 'sym:shared_needle file:src/deep/'"
    );

    let none = stateless_scan::search_root(
        &root,
        &cache,
        &Query::parse("sym:absent_needle")?,
        &InvocationDirectory::root(),
        &deadline,
        options(),
        ReadCutoff::after_reads(&deadline, 0),
    )?;
    assert!(none.hits.is_empty());
    assert_eq!(none.coverage["absence"], "not_proven");
    assert!(!cache.exists());
    Ok(())
}

#[test]
fn invocation_directory_files_are_read_before_the_rest() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (root, cache) = create_root(directory.path())?;
    write_many(&root, "a", 3, "pub fn shared_needle() {}\n")?;
    write_many(&root, "z/here", 3, "pub fn shared_needle() {}\n")?;
    let deadline = deadline();
    let result = stateless_scan::search_root(
        &root,
        &cache,
        &Query::parse("sym:shared_needle")?,
        &InvocationDirectory::within(&root, &root.join("z/here")),
        &deadline,
        options(),
        ReadCutoff::after_reads(&deadline, 2),
    )?;
    assert_eq!(result.hits.len(), 2);
    assert!(
        result
            .hits
            .iter()
            .all(|hit| hit.path.starts_with("z/here/"))
    );
    assert_eq!(
        result.coverage["next_step"],
        "search 'sym:shared_needle file:z/here/'"
    );
    Ok(())
}

#[test]
fn literal_prefilter_completes_a_large_tree_extracting_only_matching_files() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (root, cache) = create_root(directory.path())?;
    write_many(
        &root,
        "bulk",
        10_000,
        "pub fn unrelated_source() -> u8 { 1 }\n",
    )?;
    fs::write(
        root.join("bulk/target_file.rs"),
        "pub fn rare_needle() {}\n",
    )?;
    for text in ["sym:rare_needle", "rare_needle"] {
        let result = search(
            &root,
            &cache,
            &Query::parse(text)?,
            &InvocationDirectory::root(),
            &QueryDeadline::after(Duration::from_secs(60)),
            options(),
        )?;
        assert!(
            result
                .hits
                .iter()
                .any(|hit| hit.path == "bulk/target_file.rs"),
            "{text}: {:?}",
            result.hits
        );
        let current = &result.coverage["current_source"];
        assert_eq!(result.coverage["complete"], true, "{text}: {current}");
        assert_eq!(current["read_files"], 1, "{text}");
        assert_eq!(current["prefiltered_files"], 10_000, "{text}");
        assert_eq!(current["walked_files"], 10_001, "{text}");
    }
    assert!(!cache.exists());
    Ok(())
}

#[test]
fn file_kind_path_matches_fill_the_page_without_reading_bodies() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (root, cache) = create_root(directory.path())?;
    write_many(&root, "other", 1_000, "manifest mentioned in a body\n")?;
    fs::create_dir_all(root.join("data"))?;
    let large = format!("{{\"manifest\": \"{}\"}}\n", "x".repeat(1024 * 1024));
    fs::write(root.join("data/manifest.json"), &large)?;
    let page_one = CurrentSearchOptions {
        page_limit: 1,
        ..options()
    };
    let result = search(
        &root,
        &cache,
        &Query::parse("manifest kind:file")?,
        &InvocationDirectory::root(),
        &deadline(),
        page_one,
    )?;
    assert_eq!(result.hits[0].path, "data/manifest.json");
    assert!(result.hits[0].handle.starts_with("read:path:"));
    let current = &result.coverage["current_source"];
    assert_eq!(current["body_lane"], "skipped");
    assert_eq!(current["facts_indexed"], 0);
    assert_eq!(
        current["read_files"], 1,
        "only the hit was read for its handle"
    );
    assert!(result.coverage["hint"].as_str().unwrap().contains("bodies"));

    let result = search(
        &root,
        &cache,
        &Query::parse("manifest kind:file file:data/ file:other/f00001.rs file:other/f00002.rs")?,
        &InvocationDirectory::root(),
        &deadline(),
        options(),
    )?;
    assert_eq!(
        result.hits[0].path, "data/manifest.json",
        "path match leads"
    );
    assert!(
        result.hits.len() > 1,
        "body matches fill in: {:?}",
        result.hits
    );
    assert_eq!(result.coverage["current_source"]["body_lane"], "complete");
    assert!(!cache.exists());
    Ok(())
}

#[test]
fn stateless_refs_report_sites_and_partial_coverage() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (root, cache) = create_root(directory.path())?;
    fs::create_dir_all(root.join("src/use"))?;
    fs::write(root.join("src/def.rs"), "pub fn ref_target() {}\n")?;
    for index in 0..4 {
        fs::write(
            root.join(format!("src/use/u{index}.rs")),
            "fn call() { crate::ref_target(); }\n",
        )?;
    }
    write_many(&root, "noise", 50, "fn unrelated() {}\n")?;
    let complete = refs(
        &root,
        &cache,
        "ref_target",
        &InvocationDirectory::root(),
        &deadline(),
    )?;
    assert_eq!(complete.hits.len(), 5, "{:?}", complete.hits);
    assert!(
        complete
            .hits
            .iter()
            .all(|hit| hit.handle.starts_with("read:path:"))
    );
    assert_eq!(complete.coverage["complete"], true);
    assert_eq!(complete.coverage["current_source"]["read_files"], 5);
    assert_eq!(complete.coverage["reference_sites"], 5);

    let deadline = deadline();
    let partial = current_refs::refs_root(
        &root,
        &cache,
        "ref_target file:src/",
        &InvocationDirectory::root(),
        &deadline,
        ReadCutoff::after_reads(&deadline, 2),
    )?;
    assert_eq!(partial.hits.len(), 2);
    assert_eq!(partial.coverage["complete"], false);
    assert_eq!(partial.coverage["current_source"]["unsearched_files"], 3);
    assert_eq!(
        partial.coverage["next_step"],
        "refs ref_target file:src/use/"
    );
    assert!(!cache.exists());
    Ok(())
}

#[test]
fn directory_map_outlines_files_and_reports_a_partial_remainder() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (root, cache) = create_root(directory.path())?;
    fs::create_dir_all(root.join("src/stream/inner"))?;
    fs::write(
        root.join("src/stream/a.rs"),
        "pub struct Alpha;\npub fn alpha() {}\n",
    )?;
    fs::write(root.join("src/stream/b.rs"), "pub enum Beta { One }\n")?;
    fs::write(root.join("src/stream/inner/c.rs"), "pub fn gamma() {}\n")?;
    fs::write(root.join("src/other.rs"), "pub fn outside() {}\n")?;
    let complete = map_path(&root, &cache, "src/stream/", &deadline())?;
    let rows: Vec<_> = complete
        .hits
        .iter()
        .map(|hit| (hit.path.as_str(), hit.kind.as_str(), hit.name.as_str()))
        .collect();
    assert_eq!(
        rows,
        [
            ("src/stream/a.rs", "file", "Alpha, alpha"),
            ("src/stream/b.rs", "file", "Beta"),
            ("src/stream/inner/c.rs", "file", "gamma"),
        ]
    );
    assert!(
        complete
            .hits
            .iter()
            .all(|hit| hit.handle.starts_with("read:path:"))
    );
    assert_eq!(complete.coverage["complete"], true);

    let deadline = deadline();
    let partial = current_map::map_current_path(
        &root,
        &cache,
        "src/stream",
        &deadline,
        ReadCutoff::after_reads(&deadline, 2),
    )?;
    assert_eq!(partial.hits.len(), 2);
    assert_eq!(partial.coverage["complete"], false);
    assert_eq!(partial.coverage["next_step"], "map src/stream/inner");
    assert!(
        partial.coverage["hint"]
            .as_str()
            .unwrap()
            .contains("map a subdirectory")
    );
    assert!(!cache.exists());
    Ok(())
}
