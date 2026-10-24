use super::*;
use crate::workspace::test_fixture::Fixture;
use std::{fs, time::Duration};

#[test]
fn cache_free_workspace_hits_keep_their_owner_without_any_cache_files() {
    let fixture = Fixture::new();
    let page = fixture.json(
        "engine",
        &[
            "--no-cache",
            "--budget",
            "4000",
            "search",
            "sym:SharedThing ws:all",
        ],
    );
    assert_eq!(page["hits"].as_array().unwrap().len(), 3, "{page}");
    assert!(page.get("id").is_none());
    assert!(page.get("next").is_none_or(Value::is_null));
    assert_eq!(fs::read_dir(fixture.cache.path()).unwrap().count(), 0);
    let hit = page["hits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| hit["member"] == "pack")
        .unwrap();
    let handle = hit["handle"].as_str().unwrap();
    assert!(handle.starts_with("read:path:"), "{hit}");
    let shown = fixture.json("engine", &["show", handle]);
    assert_eq!(shown["member"], "pack", "{shown}");
    assert!(shown.to_string().contains("pack_marker"), "{shown}");
    assert_eq!(fs::read_dir(fixture.cache.path()).unwrap().count(), 0);
    let wrong_owner = fixture
        .run("engine", &["--member", "engine", "show", handle])
        .unwrap_err();
    assert!(
        wrong_owner.to_string().contains("invalid_cursor"),
        "{wrong_owner:#}"
    );
    fixture.configure(&["engine", "upstream"]);
    let removed = fixture.run("engine", &["show", handle]).unwrap_err();
    assert!(
        removed.to_string().contains("invalid_cursor"),
        "{removed:#}"
    );
}

#[test]
fn cache_free_worktree_map_reads_changed_source_and_rejects_stale_handle() {
    let fixture = Fixture::git_backed();
    let location = fixture.root.path().join("engine-worktree");
    let worktree = fixture.worktree("engine", &location);
    fs::write(worktree.join("lib.rs"), "pub fn worktree_only() {}\n").unwrap();
    let output = fixture
        .run_at(
            &worktree,
            &["--no-cache", "--budget", "4000", "map", "lib.rs"],
        )
        .unwrap();
    let page: Value = serde_json::from_str(&output).unwrap();
    assert!(page.to_string().contains("worktree_only"), "{page}");
    let handle = page["hits"][0]["handle"].as_str().unwrap();
    let shown = fixture.run_at(&worktree, &["show", handle]).unwrap();
    assert!(shown.contains("worktree_only"), "{shown}");
    fs::write(worktree.join("lib.rs"), "pub fn worktree_newer() {}\n").unwrap();
    let stale = fixture.run_at(&worktree, &["show", handle]).unwrap_err();
    assert!(format!("{stale:#}").contains("stale_source"), "{stale:#}");
    assert_eq!(fs::read_dir(fixture.cache.path()).unwrap().count(), 0);
}

#[test]
fn workspace_cache_refusal_returns_current_hits_and_preserves_the_selected_path() {
    let fixture = Fixture::new();
    let config = WorkspaceConfig::load(&fixture.config).unwrap();
    let selected = super::super::cache_path(&config, Some(fixture.cache.path())).unwrap();
    fs::create_dir_all(selected.parent().unwrap()).unwrap();
    fs::write(&selected, b"selected cache is a file").unwrap();
    let page = fixture.json("engine", &["--budget", "4000", "search", "sym:SharedThing"]);
    assert_eq!(page["hits"].as_array().unwrap().len(), 1, "{page}");
    assert_eq!(page["cache_fallback"]["status"], "index_cache_unavailable");
    assert_eq!(page["cache_fallback"]["cache"], encode_path(&selected));
    assert_eq!(fs::read(&selected).unwrap(), b"selected cache is a file");
    let handle = page["hits"][0]["handle"].as_str().unwrap();
    assert!(
        fixture
            .run("engine", &["show", handle])
            .unwrap()
            .contains("engine_marker")
    );
}

#[test]
fn member_cache_refusal_is_detected_before_indexed_retrieval() {
    let fixture = Fixture::new();
    let selected = fixture.member_cache("engine");
    fs::create_dir_all(selected.parent().unwrap()).unwrap();
    fs::write(&selected, b"member cache marker").unwrap();
    let page = fixture.json("engine", &["--budget", "4000", "map", "lib.rs"]);
    assert!(!page["hits"].as_array().unwrap().is_empty(), "{page}");
    assert_eq!(page["cache_fallback"]["cache"], encode_path(&selected));
    assert_eq!(fs::read(&selected).unwrap(), b"member cache marker");
    let config = WorkspaceConfig::load(&fixture.config).unwrap();
    let workspace_cache = super::super::cache_path(&config, Some(fixture.cache.path())).unwrap();
    if workspace_cache.exists() {
        assert_eq!(fs::read_dir(workspace_cache).unwrap().count(), 0);
    }
}

#[test]
fn expired_cache_free_workspace_request_does_not_probe_or_scan() {
    let fixture = Fixture::new();
    let config = WorkspaceConfig::load(&fixture.config).unwrap();
    let options = crate::cli::parse(&[
        "--root".into(),
        fixture.root.path().join("engine").display().to_string(),
        "--no-cache".into(),
        "search".into(),
        "sym:SharedThing".into(),
    ])
    .unwrap();
    let error = run(
        &config,
        &options,
        &QueryDeadline::after(Duration::ZERO),
        None,
    )
    .unwrap_err();
    assert!(crate::daemon::deadline::is_timed_out(&error), "{error:#}");
    assert_eq!(fs::read_dir(fixture.cache.path()).unwrap().count(), 0);
}
