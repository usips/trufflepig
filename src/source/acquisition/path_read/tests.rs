use super::*;
use crate::{output::OutputBudget, search::InvocationDirectory, source::show, store::Store};
use serde_json::Value;
use std::{
    ffi::OsStr,
    fs,
    os::unix::{ffi::OsStrExt, fs::symlink},
    path::{Path, PathBuf},
};
use tempfile::{Builder, TempDir};

mod cursor_scope_tests;
mod save_failure_tests;

fn test_directory() -> TempDir {
    Builder::new().prefix("path-read-").tempdir().unwrap()
}

fn blocked_cache(parent: &Path) -> PathBuf {
    let blocker = parent.join("cache-is-a-file");
    fs::write(&blocker, "not a directory").unwrap();
    blocker.join("results")
}

fn source_bytes(lines: usize) -> Vec<u8> {
    (0..lines)
        .flat_map(|line| format!("line-{line:03}\n").into_bytes())
        .collect()
}

fn page(store: &Store, target: &str) -> Value {
    let output = show(store, target, &OutputBudget::new(20_000).unwrap()).unwrap();
    serde_json::from_str(&output).unwrap()
}

fn root_digest(root: &Path) -> String {
    blake3::hash(root.canonicalize().unwrap().as_os_str().as_bytes())
        .to_hex()
        .to_string()
}

fn stateless_cursor(
    root: &Path,
    path: &str,
    start: usize,
    end: usize,
    revision: &str,
    offset: usize,
) -> String {
    format!(
        "read:path:{}:{path}:{start}-{end}@{revision}@{offset}",
        root_digest(root)
    )
}

#[test]
fn blocked_cache_uses_revision_bound_continuation_across_stores() {
    let directory = test_directory();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    let bytes = source_bytes(260);
    fs::write(root.join("src.rs"), &bytes).unwrap();
    let cache = blocked_cache(directory.path());
    let first_store = Store::unpublished(&root, &cache).unwrap();

    let first = page(&first_store, "path:src.rs:2-250");
    let start = 9;
    let end = 250 * 9;
    let revision = ContentRevision::of(&bytes).to_string();
    let offset = 9 + 200 * 9;
    let expected = stateless_cursor(&root, "src.rs", start, end, &revision, offset);
    assert!(end < bytes.len());
    assert_eq!(first["start"], start);
    assert_eq!(first["end"], end);
    assert_eq!(first["revision"], revision);
    assert_eq!(first["verified"], false);
    assert_eq!(first["source"], "current_file");
    assert_eq!(first["lines"].as_array().unwrap().len(), 200);
    assert_eq!(first["lines"][199]["end"], offset);
    assert_eq!(first["next"], expected);

    let second_cache = directory.path().join("never-created-cache");
    let second_store = Store::unpublished(&root, &second_cache).unwrap();
    assert!(!second_cache.exists());
    let second = page(&second_store, &expected);
    assert_eq!(second["start"], offset);
    assert_eq!(second["end"], end);
    assert_eq!(second["revision"], revision);
    assert_eq!(second["verified"], false);
    assert_eq!(second["source"], "current_file");
    assert_eq!(second["lines"][0]["start"], offset);
    assert_eq!(second["lines"].as_array().unwrap().len(), 49);
    assert!(second["next"].is_null());
    assert!(!second_cache.exists());
}

#[test]
fn missing_cache_is_not_created_until_saved_path_read_and_saved_cursor_still_works() {
    let directory = test_directory();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    let bytes = source_bytes(205);
    fs::write(root.join("src.rs"), &bytes).unwrap();
    let cache = directory.path().join("optional-cache");
    let first_store = Store::unpublished(&root, &cache).unwrap();
    assert!(!cache.exists());

    let first = page(&first_store, "path:src.rs");
    let cursor = first["next"].as_str().unwrap();
    assert!(cursor.starts_with("read:"));
    assert!(!cursor.starts_with("read:path:"));
    assert!(cache.is_dir());

    let second_store = Store::unpublished(&root, &cache).unwrap();
    let second = page(&second_store, cursor);
    assert_eq!(second["verified"], true);
    assert!(second["source"].is_null());
    assert_eq!(second["revision"], ContentRevision::of(&bytes).to_string());
}

#[test]
fn cache_independent_acquisition_skips_optional_result_storage() {
    let directory = test_directory();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("src.rs"), source_bytes(205)).unwrap();
    let cache = directory.path().join("optional-cache");
    let store = Store::unpublished(&root, &cache).unwrap();
    assert!(!cache.exists());

    let acquired = super::super::acquire_current_path_without_cache(
        &store,
        "path:src.rs",
        None,
        &InvocationDirectory::root(),
    )
    .unwrap();
    assert!(
        acquired
            .handle
            .starts_with(&format!("path:{}:src.rs:", root_digest(&root)))
    );
    let output = crate::source::render_owned(
        acquired,
        &OutputBudget::new(20_000).unwrap(),
        &serde_json::json!({}),
    )
    .unwrap();
    let page: Value = serde_json::from_str(&output).unwrap();
    assert!(
        page["next"]
            .as_str()
            .unwrap()
            .starts_with(&format!("read:path:{}:src.rs:", root_digest(&root)))
    );
    assert_eq!(page["verified"], false);
    assert_eq!(page["source"], "current_file");
    assert!(!cache.exists());
}

#[test]
fn current_hit_handle_reads_revision_bound_lines_without_cache() {
    let directory = test_directory();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    let bytes = b"first line\nneedle in the current source\nlast line\n";
    fs::write(root.join("src.rs"), bytes).unwrap();
    let cache = directory.path().join("never-created-cache");
    let canonical_root = root.canonicalize().unwrap();
    let needle = bytes
        .windows(b"needle".len())
        .position(|window| window == b"needle")
        .unwrap();
    let handle = stateless_hit_handle(
        &canonical_root,
        Path::new("src.rs"),
        bytes,
        needle,
        needle + b"needle".len(),
    )
    .unwrap();

    assert!(handle.starts_with("read:path:"));
    assert!(cursor_matches_root(&handle, &canonical_root).unwrap());
    let other_root = directory.path().join("other");
    fs::create_dir_all(&other_root).unwrap();
    assert!(!cursor_matches_root(&handle, &other_root).unwrap());

    let store = Store::unpublished(&root, &cache).unwrap();
    let shown = page(&store, &handle);
    assert_eq!(shown["source"], "current_file");
    assert_eq!(shown["verified"], false);
    assert_eq!(shown["lines"][0]["text"], "needle in the current source\n");
    assert!(!cache.exists());

    fs::write(root.join("src.rs"), b"replacement source\n").unwrap();
    let changed_store = Store::unpublished(&root, &cache).unwrap();
    let error = show(&changed_store, &handle, &OutputBudget::new(20_000).unwrap()).unwrap_err();
    assert!(error.to_string().starts_with("stale_source:"));
    assert!(!cache.exists());
}

#[test]
fn stateless_continuation_rejects_changed_current_bytes() {
    let directory = test_directory();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    let bytes = source_bytes(205);
    fs::write(root.join("src.rs"), &bytes).unwrap();
    let cache = blocked_cache(directory.path());
    let store = Store::unpublished(&root, &cache).unwrap();
    let first = page(&store, "path:src.rs");
    let cursor = first["next"].as_str().unwrap().to_owned();

    fs::write(root.join("src.rs"), b"changed bytes\n").unwrap();
    let resumed_store = Store::unpublished(&root, &cache).unwrap();
    let error = show(&resumed_store, &cursor, &OutputBudget::new(20_000).unwrap()).unwrap_err();
    assert!(error.to_string().starts_with("stale_source:"));
}

#[test]
fn stateless_identity_encodes_delimiters_and_non_utf8_filenames() {
    let directory = test_directory();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    let bytes = source_bytes(205);
    let cache = blocked_cache(directory.path());
    let store = Store::unpublished(&root, &cache).unwrap();
    let names: [&[u8]; 2] = [b"special:@%.txt", b"nonutf8-\xff.txt"];

    for name in names {
        let relative = PathBuf::from(OsStr::from_bytes(name));
        fs::write(root.join(&relative), &bytes).unwrap();
        let encoded = crate::path_codec::encode_path(&relative);
        assert!(!encoded.contains(':'));
        assert!(!encoded.contains('@'));
        if name == b"special:@%.txt" {
            assert_eq!(encoded, "special%3A%40%25.txt");
        } else {
            assert_eq!(encoded, "nonutf8-%FF.txt");
        }

        let first = page(&store, &format!("path:{encoded}:1-205"));
        let cursor = first["next"].as_str().unwrap();
        assert!(cursor.starts_with(&format!("read:path:{}:{encoded}:", root_digest(&root))));
        assert!(cursor.contains(&format!("@{}@", ContentRevision::of(&bytes))));

        let resumed_store = Store::unpublished(&root, &cache).unwrap();
        let second = page(&resumed_store, cursor);
        assert_eq!(second["path"], encoded);
        assert_eq!(second["source"], "current_file");
        assert_eq!(second["verified"], false);
    }
}

#[test]
fn traversal_symlink_and_malformed_or_out_of_range_cursors_are_rejected() {
    let directory = test_directory();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    let bytes = b"alpha\nb\xc3\xa9ta\ngamma\n";
    fs::write(root.join("safe.txt"), bytes).unwrap();
    let outside = directory.path().join("outside.txt");
    fs::write(&outside, bytes).unwrap();
    symlink(&outside, root.join("alias.txt")).unwrap();
    let cache = blocked_cache(directory.path());
    let store = Store::unpublished(&root, &cache).unwrap();
    let budget = OutputBudget::new(20_000).unwrap();

    assert!(show(&store, "path:../outside.txt", &budget).is_err());
    assert!(show(&store, "path:alias.txt", &budget).is_err());

    let revision = ContentRevision::of(bytes).to_string();
    let traversal = stateless_cursor(&root, "../outside.txt", 0, bytes.len(), &revision, 0);
    assert!(show(&store, &traversal, &budget).is_err());
    let symlink_cursor = stateless_cursor(&root, "alias.txt", 0, bytes.len(), &revision, 0);
    assert!(show(&store, &symlink_cursor, &budget).is_err());

    let identity = root_digest(&root);
    let malformed_cursors = [
        format!("read:path:safe.txt:0-12@{revision}@0"),
        format!("read:path:{identity}:safe.txt:0-nope@{revision}@0"),
        format!("read:path:{identity}:safe.txt:0-12@0"),
        format!("read:path:{identity}:safe.txt:0@{revision}@0"),
        format!("read:path:{identity}:safe.txt:0-12@@0"),
        format!("read:path:{identity}:safe.txt:0-12@{revision}@extra@0"),
    ];
    for cursor in malformed_cursors {
        let error = show(&store, &cursor, &budget).unwrap_err();
        assert!(
            error.to_string().starts_with("invalid_cursor:"),
            "{cursor}: {error}"
        );
    }
    let out_of_range = stateless_cursor(&root, "safe.txt", 0, 6, &revision, 7);
    let error = show(&store, &out_of_range, &budget).unwrap_err();
    assert!(error.to_string().starts_with("invalid_cursor:"));
    let invalid_original_span =
        stateless_cursor(&root, "safe.txt", 0, bytes.len() + 1, &revision, 0);
    let error = show(&store, &invalid_original_span, &budget).unwrap_err();
    assert!(error.to_string().starts_with("invalid_cursor:"));
    let mid_line = stateless_cursor(&root, "safe.txt", 0, 6, &revision, 1);
    let error = show(&store, &mid_line, &budget).unwrap_err();
    assert!(error.to_string().starts_with("invalid_cursor:"));
    let mid_utf8 = stateless_cursor(&root, "safe.txt", 0, 12, &revision, 8);
    let error = show(&store, &mid_utf8, &budget).unwrap_err();
    assert!(error.to_string().starts_with("invalid_cursor:"));
}

#[test]
fn current_path_fallback_classification_excludes_symbols_and_saved_handles() {
    assert!(super::super::is_current_path_target("src/lib.rs"));
    assert!(super::super::is_current_path_target(
        "path:src/lib.rs:10-20"
    ));
    assert!(super::super::is_current_path_target(
        "read:path:src/lib.rs:0-100@abcd@20"
    ));
    assert!(!super::super::is_current_path_target("sym:parse_item"));
    assert!(!super::super::is_current_path_target(
        "read:result-set:1@20"
    ));
    assert!(!super::super::is_current_path_target(
        "read:more:result-set@20"
    ));
    assert!(!super::super::is_current_path_target(
        "00000000-0000-0000-0000-000000000001:1"
    ));
    assert!(!super::super::is_current_path_target(
        "00000000-0000-0000-0000-000000000001:arbitrary"
    ));
}
