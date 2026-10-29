use super::*;
use crate::workspace::test_fixture::Fixture;
use serde_json::Value;
use std::{fs, time::Duration};
use tp_core::coded_error::ErrorCode;

#[test]
fn cache_free_symbol_show_retains_the_selected_member_and_creates_no_cache() {
    let fixture = Fixture::new();
    let shown = fixture.json(
        "engine",
        &["--no-cache", "--member", "pack", "show", "sym:SharedThing"],
    );
    assert_eq!(shown["member"], "pack", "{shown}");
    assert_eq!(shown["definitions"], 1);
    assert_eq!(shown["coverage"]["current_source"]["complete"], true);
    assert!(shown.to_string().contains("pack_marker"), "{shown}");
    assert_eq!(fs::read_dir(fixture.cache.path()).unwrap().count(), 0);
}

#[test]
fn cache_free_symbol_miss_stays_in_its_owner_instead_of_widening() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.path().join("engine/lib.rs"),
        "pub struct EngineOnly;\n",
    )
    .unwrap();
    let error = fixture
        .run("engine", &["--no-cache", "show", "sym:SharedThing"])
        .unwrap_err();
    assert!(ErrorCode::NoDefinition.is_code_of(&error), "{error:#}");
    assert_eq!(fs::read_dir(fixture.cache.path()).unwrap().count(), 0);
}

#[test]
fn workspace_symbol_show_falls_back_when_workspace_results_are_unavailable() {
    let fixture = Fixture::new();
    let config = WorkspaceConfig::load(&fixture.config).unwrap();
    let selected = cache_path(&config, Some(fixture.cache.path())).unwrap();
    fs::create_dir_all(selected.parent().unwrap()).unwrap();
    fs::write(&selected, "keep workspace cache").unwrap();
    let shown = fixture.json("engine", &["show", "sym:SharedThing"]);
    assert_eq!(shown["member"], "engine", "{shown}");
    assert!(shown.to_string().contains("engine_marker"), "{shown}");
    assert_eq!(shown["cache_fallback"]["status"], "index_cache_unavailable");
    assert_eq!(shown["cache_fallback"]["cache"], encode_path(&selected));
    assert_eq!(
        fs::read_to_string(&selected).unwrap(),
        "keep workspace cache"
    );
}

#[test]
fn workspace_symbol_show_falls_back_for_its_selected_member_cache() {
    let fixture = Fixture::new();
    let selected = fixture.member_cache("pack");
    fs::create_dir_all(selected.parent().unwrap()).unwrap();
    fs::write(&selected, "keep member cache").unwrap();
    let shown = fixture.json("engine", &["--member", "pack", "show", "sym:SharedThing"]);
    assert_eq!(shown["member"], "pack", "{shown}");
    assert!(shown.to_string().contains("pack_marker"), "{shown}");
    assert_eq!(shown["cache_fallback"]["cache"], encode_path(&selected));
    assert_eq!(fs::read_to_string(&selected).unwrap(), "keep member cache");
}

#[test]
fn workspace_symbol_show_falls_back_when_member_index_cannot_open() {
    let fixture = Fixture::new();
    let cache = fixture.member_cache("pack");
    fs::create_dir_all(cache.join("index.sqlite3")).unwrap();
    let shown = fixture.json("engine", &["--member", "pack", "show", "sym:SharedThing"]);
    assert_eq!(shown["member"], "pack", "{shown}");
    assert!(shown.to_string().contains("pack_marker"), "{shown}");
    assert_eq!(shown["cache_fallback"]["status"], "index_cache_unavailable");
    assert!(cache.join("index.sqlite3").is_dir());
}

#[test]
fn late_workspace_refusal_retains_provenance_without_guessing_cache_owner() {
    let fixture = Fixture::new();
    let config = WorkspaceConfig::load(&fixture.config).unwrap();
    let workspace_cache = cache_path(&config, Some(fixture.cache.path())).unwrap();
    let options = crate::cli::parse(&[
        "--root".into(),
        fixture.root.path().join("engine").display().to_string(),
        "--cache".into(),
        fixture.cache.path().display().to_string(),
        "show".into(),
        "sym:SharedThing".into(),
    ])
    .unwrap();
    let error = tp_index::store::index_cache_unavailable(
        &workspace_cache,
        std::io::Error::from(std::io::ErrorKind::PermissionDenied).into(),
    );
    let shown: Value = serde_json::from_str(
        &read_after_refusal(&config, &options, &QueryDeadline::start(), error).unwrap(),
    )
    .unwrap();
    assert_eq!(shown["member"], "engine");
    assert_eq!(shown["cache_fallback"]["status"], "index_cache_unavailable");
    assert!(
        shown["cache_fallback"]["reason"]
            .as_str()
            .unwrap()
            .contains(&encode_path(&workspace_cache))
    );
    assert!(
        shown["cache_fallback"]["reason"]
            .as_str()
            .unwrap()
            .contains("permission denied")
    );
    assert!(shown["cache_fallback"].get("cache").is_none());
    assert_eq!(fs::read_dir(fixture.cache.path()).unwrap().count(), 0);
}

#[test]
fn cache_free_worktree_symbol_show_preserves_its_cursor_owner() {
    let fixture = Fixture::git_backed();
    let parent = fixture.root.path().join("engine");
    fs::write(parent.join(".git/info/exclude"), ".worktrees/\n").unwrap();
    let worktree = fixture.worktree("engine", &parent.join(".worktrees/symbol"));
    let fields = (0..260)
        .map(|n| format!("    pub worktree_{n:03}: u64,\n"))
        .collect::<String>();
    fs::write(
        worktree.join("lib.rs"),
        format!("pub struct SharedThing {{\n{fields}}}\n"),
    )
    .unwrap();
    let shown: Value = serde_json::from_str(
        &fixture
            .run_at(
                &worktree,
                &["--no-cache", "--budget", "4000", "show", "sym:SharedThing"],
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(shown["member"], "engine", "{shown}");
    assert_eq!(shown["repository"], encode_path(&worktree));
    assert!(shown.get("worktree").is_some(), "{shown}");
    assert!(shown.to_string().contains("worktree_000"), "{shown}");
    let next = shown["next"]
        .as_str()
        .expect("large worktree symbol continues");
    let continued: Value =
        serde_json::from_str(&fixture.run_at(&worktree, &["show", next]).unwrap()).unwrap();
    assert_eq!(continued["member"], "engine", "{continued}");
    assert_eq!(continued["revision"], shown["revision"]);
    let error = fixture.run("pack", &["show", next]).unwrap_err();
    assert!(ErrorCode::InvalidCursor.is_code_of(&error), "{error:#}");
    let error = fixture
        .run_at(&worktree, &["--member", "pack", "show", next])
        .unwrap_err();
    assert!(ErrorCode::InvalidCursor.is_code_of(&error), "{error:#}");
    assert_eq!(fs::read_dir(fixture.cache.path()).unwrap().count(), 0);
}

#[test]
fn expired_cache_free_symbol_show_does_not_probe_selected_caches() {
    let fixture = Fixture::new();
    let config = WorkspaceConfig::load(&fixture.config).unwrap();
    let options = crate::cli::parse(&[
        "--root".into(),
        fixture.root.path().join("engine").display().to_string(),
        "--no-cache".into(),
        "show".into(),
        "sym:SharedThing".into(),
    ])
    .unwrap();
    let error =
        read_if_cache_free(&config, &options, &QueryDeadline::after(Duration::ZERO)).unwrap_err();
    assert!(ErrorCode::TimedOut.is_code_of(&error), "{error:#}");
    assert_eq!(fs::read_dir(fixture.cache.path()).unwrap().count(), 0);
}
