use super::{Value, args, cli, fs};
use tp_core::{coded_error::ErrorCode, query_deadline::QueryDeadline};

#[test]
fn cache_free_symbol_emission_continues_without_cache_and_rejects_source_edits() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().join("repo");
    let cache = scratch.path().join("absent-cache");
    fs::create_dir(&root).unwrap();
    let fields = (0..260)
        .map(|n| format!("    pub field_{n:03}: u64,\n"))
        .collect::<String>();
    let bytes = format!("pub struct LocatedHere {{\n{fields}}}\n");
    fs::write(root.join("lib.rs"), &bytes).unwrap();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let status = cli::emission::execute(
        &args(&root, &cache, &["--no-cache", "show", "sym:LocatedHere"]),
        &mut stdout,
        &mut stderr,
    );
    assert_eq!(status, 0, "{}", String::from_utf8_lossy(&stderr));
    let shown: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(shown["path"], "lib.rs");
    assert_eq!(shown["definitions"], 1);
    assert_eq!(shown["coverage"]["current_source"]["complete"], true);
    let next = shown["next"].as_str().expect("large definitions continue");
    assert!(next.starts_with("read:path:"), "{shown}");
    let continued: Value = serde_json::from_str(
        &cli::run(&args(&root, &cache, &["--no-cache", "show", next])).unwrap(),
    )
    .unwrap();
    assert_eq!(
        continued["start"],
        shown["lines"].as_array().unwrap().last().unwrap()["end"]
    );
    assert_eq!(continued["revision"], shown["revision"]);
    assert!(
        !cache.exists(),
        "no-cache source reads must not create diagnostics or result caches"
    );

    fs::write(root.join("lib.rs"), "pub struct LocatedHere;\n").unwrap();
    let error = cli::run(&args(&root, &cache, &["--no-cache", "show", next])).unwrap_err();
    assert!(ErrorCode::StaleSource.is_code_of(&error), "{error:#}");
    assert!(!cache.exists());
}

#[test]
fn symbol_cache_refusal_falls_back_before_daemon_startup() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().join("repo");
    let cache = scratch.path().join("selected-cache");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("lib.rs"), "pub struct LocatedHere;\n").unwrap();
    fs::write(&cache, "preserve this selected cache").unwrap();
    for extra in [
        vec!["show", "sym:LocatedHere"],
        vec!["--no-daemon", "show", "sym:LocatedHere"],
    ] {
        let options = cli::parse(&args(&root, &cache, &extra)).unwrap();
        let response = cli::direct_with_deadline(
            &options,
            &tp_core::request_context::RequestContext::new(None, None),
            QueryDeadline::start(),
        )
        .unwrap();
        let shown: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(shown["path"], "lib.rs");
        assert_eq!(shown["definitions"], 1);
        assert_eq!(shown["cache_fallback"]["status"], "index_cache_unavailable");
        assert_eq!(
            shown["cache_fallback"]["cache"],
            cache.display().to_string()
        );
        assert_eq!(
            fs::read_to_string(&cache).unwrap(),
            "preserve this selected cache"
        );
    }
}

#[test]
fn symbol_show_falls_back_when_index_open_fails_after_results_preflight() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().join("repo");
    let cache = scratch.path().join("writable-cache");
    fs::create_dir(&root).unwrap();
    fs::create_dir_all(cache.join("index.sqlite3")).unwrap();
    fs::write(root.join("lib.rs"), "pub struct LocatedHere;\n").unwrap();
    let response = cli::run(&args(
        &root,
        &cache,
        &["--no-daemon", "show", "sym:LocatedHere"],
    ))
    .unwrap();
    let shown: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(shown["path"], "lib.rs", "{shown}");
    assert_eq!(shown["cache_fallback"]["status"], "index_cache_unavailable");
    assert!(cache.join("index.sqlite3").is_dir());
}

#[test]
fn cache_free_symbol_show_rejects_historical_side_without_cache_access() {
    let scratch = tempfile::tempdir().unwrap();
    let cache = scratch.path().join("absent-cache");
    let error = cli::run(&args(
        scratch.path(),
        &cache,
        &["--no-cache", "--side", "before", "show", "sym:LocatedHere"],
    ))
    .unwrap_err();
    assert!(ErrorCode::InvalidSide.is_code_of(&error), "{error:#}");
    assert!(!cache.exists());
}

#[test]
fn cache_free_symbol_show_excludes_sibling_default_caches_inside_source() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().canonicalize().unwrap();
    let base = root.join(".cache/trufflepig");
    let cache = base.join("selected-member-cache");
    let sibling = base.join("workspaces/other-workspace");
    fs::create_dir_all(&sibling).unwrap();
    fs::write(root.join("lib.rs"), "pub struct LocatedHere;\n").unwrap();
    let sibling_bytes = b"LocatedHere\0unrelated binary cache";
    let database = sibling.join("workspace.sqlite3");
    fs::write(&database, sibling_bytes).unwrap();
    let mut options = cli::parse(&args(
        &root,
        &cache,
        &["--no-cache", "show", "sym:LocatedHere"],
    ))
    .unwrap();
    options.cache = None;
    let response = super::super::run(&root, &cache, &options, &QueryDeadline::start()).unwrap();
    let shown: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(shown["path"], "lib.rs", "{shown}");
    assert_eq!(shown["coverage"]["current_source"]["walked_files"], 1);
    assert_eq!(shown["coverage"]["complete"], true);
    assert_eq!(fs::read(&database).unwrap(), sibling_bytes);
    assert!(!cache.exists());
}
