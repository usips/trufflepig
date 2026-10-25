use crate::cli;
use serde_json::Value;
use std::{fs, path::Path};
use tp_index::store::Store;

fn args(root: &Path, cache: &Path, extra: &[&str]) -> Vec<String> {
    let mut args = vec![
        "--root".into(),
        root.display().to_string(),
        "--cache".into(),
        cache.display().to_string(),
        "--no-workspace".into(),
        "--budget".into(),
        "4000".into(),
    ];
    args.extend(extra.iter().map(|word| (*word).to_owned()));
    args
}

#[test]
fn cache_free_emission_creates_no_cache_even_with_default_diagnostics() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().join("repo");
    let cache = scratch.path().join("absent-cache");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("lib.rs"), "pub fn explicit_locator() {}\n").unwrap();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let result = cli::emission::execute(
        &args(
            &root,
            &cache,
            &["--no-cache", "search", "sym:explicit_locator"],
        ),
        &mut stdout,
        &mut stderr,
    );
    assert_eq!(result, 0, "{}", String::from_utf8_lossy(&stderr));
    let page: Value = serde_json::from_slice(&stdout).unwrap();
    assert!(!page["hits"].as_array().unwrap().is_empty(), "{page}");
    let handle = page["hits"][0]["handle"].as_str().unwrap();
    let shown = cli::run(&args(&root, &cache, &["--no-cache", "show", handle])).unwrap();
    assert!(shown.contains("explicit_locator"), "{shown}");
    assert!(
        !cache.exists(),
        "explicit no-cache must not create diagnostics, indexes, result sets or probes"
    );
}

#[test]
fn cache_free_lookup_preserves_an_unusable_selected_cache_path() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().join("repo");
    let cache = scratch.path().join("cache-file");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("lib.rs"), "pub struct LocatedHere;\n").unwrap();
    fs::write(&cache, b"keep selected cache identity").unwrap();
    for command in [vec!["search", "sym:LocatedHere"], vec!["map", "lib.rs"]] {
        let mut extra = vec!["--no-daemon", "--diagnostics", "off"];
        extra.extend(command);
        let output = cli::run(&args(&root, &cache, &extra)).unwrap();
        let page: Value = serde_json::from_str(&output).unwrap();
        assert!(!page["hits"].as_array().unwrap().is_empty(), "{page}");
        assert_eq!(page["cache_fallback"]["cache"], cache.display().to_string());
        assert_eq!(page["cache_fallback"]["status"], "index_cache_unavailable");
        assert_eq!(fs::read(&cache).unwrap(), b"keep selected cache identity");
    }
}

#[test]
fn post_dispatch_result_corruption_does_not_start_current_source_fallback() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().join("repo");
    let cache = scratch.path().join("cache");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("lib.rs"), "pub fn indexed_locator() {}\n").unwrap();
    Store::open(&root, &cache).unwrap().index().unwrap();
    fs::write(cache.join("results.sqlite3"), b"invalid sqlite database").unwrap();
    let error = cli::run(&args(
        &root,
        &cache,
        &[
            "--no-daemon",
            "--diagnostics",
            "off",
            "search",
            "sym:indexed_locator",
        ],
    ))
    .unwrap_err();
    assert!(format!("{error:#}").contains("not a database"), "{error:#}");
    assert_eq!(
        fs::read(cache.join("results.sqlite3")).unwrap(),
        b"invalid sqlite database"
    );
}

#[test]
fn cache_free_option_rejects_index_only_commands_before_cache_access() {
    let scratch = tempfile::tempdir().unwrap();
    let cache = scratch.path().join("absent-cache");
    for command in [
        vec!["refs", "identifier"],
        vec!["show", "sym:identifier"],
        vec!["index"],
    ] {
        let mut extra = vec!["--no-cache"];
        extra.extend(command);
        let error = cli::run(&args(scratch.path(), &cache, &extra)).unwrap_err();
        assert!(error.to_string().contains("invalid_options"), "{error:#}");
        assert!(!cache.exists());
    }
}
