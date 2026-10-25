use super::*;

#[test]
fn linked_worktree_workspace_handle_reads_member_relative_source() {
    let seeded = SeededWorktree::new();
    let _writer_lease = seeded.hold_writer();
    let page = seeded.search_json(&["search", "SharedThing"]);
    let hit = &page["hits"][0];
    assert_eq!(hit["member"], "engine", "{page}");
    let handle = hit["handle"].as_str().expect("workspace hit has a handle");

    let lines = seeded.search(&["--format", "lines", "search", "SharedThing"]);
    assert!(lines.contains("engine/lib.rs"), "{lines}");
    let shown: Value = serde_json::from_str(&seeded.show(&["show", handle]).unwrap()).unwrap();
    assert_eq!(shown["path"], "lib.rs", "{shown}");
    assert_eq!(shown["verified"], true, "{shown}");
    assert!(
        shown["lines"].as_array().unwrap().iter().any(|line| {
            line["text"]
                .as_str()
                .is_some_and(|text| text.contains("worktree_marker"))
        }),
        "{shown}"
    );
}

#[test]
fn published_worktree_map_reextracts_committed_replacements() -> anyhow::Result<()> {
    let seeded = SeededWorktree::new();
    let cache = seeded.worktree_cache();
    let file = seeded.worktree.join("lib.rs");
    fs::write(&file, "pub fn old_after_pass() {}\n")?;
    git(&seeded.worktree, &["add", "lib.rs"]);
    git(&seeded.worktree, &["commit", "-q", "-m", "old outline"]);

    let mut store = Store::open(&seeded.worktree, &cache)?;
    store.index()?;
    let published_generation = store.generation()?;
    drop(store);

    fs::write(&file, "pub fn replacement_after_pass() {}\n")?;
    git(&seeded.worktree, &["add", "lib.rs"]);
    git(
        &seeded.worktree,
        &["commit", "-q", "-m", "replacement outline"],
    );

    let workspace_page = seeded.search_json(&["map", "lib.rs"]);
    let workspace_map = workspace_page.to_string();
    assert!(
        workspace_map.contains("replacement_after_pass"),
        "{workspace_map}"
    );
    assert!(!workspace_map.contains("old_after_pass"), "{workspace_map}");
    let workspace_handle = workspace_page["hits"][0]["handle"]
        .as_str()
        .expect("re-extracted workspace map hit has a handle");
    let workspace_shown: Value = serde_json::from_str(&seeded.show(&["show", workspace_handle])?)?;
    assert_eq!(workspace_shown["verified"], true, "{workspace_shown}");
    assert!(
        workspace_shown["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| {
                line["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("replacement_after_pass"))
            }),
        "{workspace_shown}"
    );

    let args = [
        "--no-workspace".to_owned(),
        "--root".to_owned(),
        seeded.worktree.display().to_string(),
        "--cache".to_owned(),
        cache.display().to_string(),
        "--diagnostics".to_owned(),
        "off".to_owned(),
        "--format".to_owned(),
        "json".to_owned(),
        "map".to_owned(),
        "lib.rs".to_owned(),
    ];
    let standalone_options = cli::parse(&args)?;
    assert!(!standalone_options.no_daemon);
    // With daemon_running set, local dispatch serves this published index
    // without reconciling it inline before retrieve_read.
    let standalone_page: Value = serde_json::from_str(&cli::local(
        &seeded.worktree,
        &cache,
        &standalone_options,
        true,
    )?)?;
    let standalone_map = standalone_page.to_string();
    assert!(
        standalone_map.contains("replacement_after_pass"),
        "{standalone_map}"
    );
    assert!(
        !standalone_map.contains("old_after_pass"),
        "{standalone_map}"
    );
    let standalone_handle = standalone_page["hits"][0]["handle"]
        .as_str()
        .expect("re-extracted standalone map hit has a handle");
    let show_args = [
        "--no-workspace".to_owned(),
        "--root".to_owned(),
        seeded.worktree.display().to_string(),
        "--cache".to_owned(),
        cache.display().to_string(),
        "--diagnostics".to_owned(),
        "off".to_owned(),
        "show".to_owned(),
        standalone_handle.to_owned(),
    ];
    let show_options = cli::parse(&show_args)?;
    let standalone_shown: Value =
        serde_json::from_str(&cli::local(&seeded.worktree, &cache, &show_options, true)?)?;
    assert_eq!(standalone_shown["verified"], true, "{standalone_shown}");
    assert!(
        standalone_shown["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| {
                line["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("replacement_after_pass"))
            }),
        "{standalone_shown}"
    );
    assert_eq!(
        Store::open(&seeded.worktree, &cache)?.generation()?,
        published_generation,
        "the direct daemon-running dispatch must preserve the stale publication"
    );
    Ok(())
}

#[test]
fn current_map_recheck_resolves_encoded_file_targets() -> anyhow::Result<()> {
    let seeded = SeededWorktree::new();
    let cache = seeded.worktree_cache();
    let file = seeded.worktree.join("space map.rs");
    fs::write(&file, "pub fn old_space_outline() {}\n")?;
    let mut store = Store::open(&seeded.worktree, &cache)?;
    store.index()?;
    fs::write(&file, "pub fn replacement_space_outline() {}\n")?;
    for target in ["space map.rs", "space%20map.rs"] {
        let mut found = tp_index::search::map(&store, target)?;
        assert!(found.hits.iter().any(|hit| hit.name == "old_space_outline"));
        assert!(crate::workspace::ParentFallback::refresh_current_map(
            &store, target, &mut found
        )?);
        assert!(
            found
                .hits
                .iter()
                .any(|hit| hit.name == "replacement_space_outline"),
            "{target}: {found:?}"
        );
        assert_eq!(found.coverage["worktree_map"]["path"], "space%20map.rs");
    }
    Ok(())
}

#[test]
fn missing_current_map_source_suppresses_unverified_outline() -> anyhow::Result<()> {
    let seeded = SeededWorktree::new();
    let cache = seeded.worktree_cache();
    let file = seeded.worktree.join("lib.rs");
    fs::write(&file, "pub fn old_after_pass() {}\n")?;
    git(&seeded.worktree, &["add", "lib.rs"]);
    git(&seeded.worktree, &["commit", "-q", "-m", "old outline"]);

    let mut store = Store::open(&seeded.worktree, &cache)?;
    store.index()?;
    fs::remove_file(&file)?;

    let mut missing = tp_index::search::map(&store, "lib.rs")?;
    assert!(
        missing.hits.iter().any(|hit| hit.name == "old_after_pass"),
        "fixture must retain the old indexed outline"
    );
    assert!(crate::workspace::ParentFallback::refresh_current_map(
        &store,
        "lib.rs",
        &mut missing
    )?);
    assert!(missing.hits.is_empty(), "{missing:?}");
    assert!(missing.truncated, "{missing:?}");
    assert_eq!(missing.coverage["worktree_map"]["status"], "unavailable");
    assert_eq!(
        missing.coverage["worktree_map"]["reason"],
        "changed source could not be outlined"
    );

    let mut unverified = tp_index::search::map(&store, "lib.rs")?;
    assert!(
        unverified
            .hits
            .iter()
            .any(|hit| hit.name == "old_after_pass"),
        "the unchanged index still contains the stale outline"
    );
    let revision = unverified.hits[0]
        .revision
        .as_deref()
        .expect("indexed map hit has a revision")
        .to_owned();
    let mut hashes = crate::workspace::WorktreeHashes::default();
    let mut exhausted = false;
    for index in 0..256 {
        let path = format!("hash-budget-{index}.rs");
        match hashes.check(&seeded.worktree, &path, Some(&revision)) {
            Some(false) => {}
            None => {
                exhausted = true;
                break;
            }
            other => panic!("missing budget path must not match: {other:?}"),
        }
    }
    assert!(exhausted, "fixture must exhaust the bounded hash budget");
    assert!(
        crate::workspace::ParentFallback::refresh_current_map_with_hashes(
            &store,
            "lib.rs",
            &mut unverified,
            &mut hashes
        )?
    );
    assert!(unverified.hits.is_empty(), "{unverified:?}");
    assert!(unverified.truncated, "{unverified:?}");
    assert_eq!(unverified.coverage["worktree_map"]["status"], "unavailable");
    assert_eq!(
        unverified.coverage["worktree_map"]["reason"],
        "current source could not be verified"
    );
    Ok(())
}
