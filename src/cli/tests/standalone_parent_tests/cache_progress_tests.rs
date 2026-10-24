use super::*;

#[test]
fn selected_read_only_worktree_cache_falls_back_without_parent_retrieval() -> Result<()> {
    const CASE: &str = "cache_progress_tests::selected_read_only_worktree_cache_falls_back_without_parent_retrieval";
    if !in_isolated_child(CASE)? {
        return Ok(());
    }

    let fixture = WorktreeFixture::new()?;
    fs::create_dir_all(&fixture.child_cache)?;
    fs::set_permissions(&fixture.child_cache, fs::Permissions::from_mode(0o555))?;
    let args = [
        "--root".to_owned(),
        fixture.worktree.display().to_string(),
        "--cache".to_owned(),
        fixture.child_cache.display().to_string(),
        "--no-daemon".to_owned(),
        "--format".to_owned(),
        "json".to_owned(),
        "--diagnostics".to_owned(),
        "off".to_owned(),
        "search".to_owned(),
        "sym:WorktreeOnlySymbol".to_owned(),
    ];
    let options = crate::cli::parse(&args)?;
    let parent_database = fixture.main_cache.join("index.sqlite3");
    let saved_database = fixture.main_cache.join("index.sqlite3.preflight-test");
    fs::rename(&parent_database, &saved_database)?;
    fs::create_dir(&parent_database)?;
    let result = crate::cli::local(&fixture.worktree, &fixture.child_cache, &options, true);
    fs::remove_dir(&parent_database)?;
    fs::rename(&saved_database, &parent_database)?;
    fs::set_permissions(&fixture.child_cache, fs::Permissions::from_mode(0o755))?;
    let text = result.expect("a read-only selected cache uses current worktree bytes");
    let page: serde_json::Value = serde_json::from_str(&text)?;
    assert_eq!(page["cache_fallback"]["status"], "index_cache_unavailable");
    assert_eq!(
        page["cache_fallback"]["cache"],
        fixture.child_cache.display().to_string()
    );
    assert_eq!(page["coverage"]["endpoint"], "stateless_current_source");
    let hits = page["hits"].as_array().expect("current-source hits");
    assert!(!hits.is_empty(), "{text}");
    let worktree = fixture.worktree.display().to_string();
    assert!(
        hits.iter()
            .all(|hit| hit["file"].as_str().unwrap().contains(&worktree))
    );
    assert!(
        !fixture.child_cache.join("results.sqlite3").exists(),
        "fallback must not save a selected-cache answer"
    );
    assert!(
        !fixture.main_cache.join("results.sqlite3").exists(),
        "fallback must not save a parent answer"
    );
    assert_eq!(
        fixture.current_parent_generation()?,
        fixture.parent_generation
    );
    Ok(())
}
