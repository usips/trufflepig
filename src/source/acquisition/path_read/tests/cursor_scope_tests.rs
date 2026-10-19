use super::{blocked_cache, page, source_bytes, test_directory};
use crate::{output::OutputBudget, source::show, store::Store};
use serde_json::Value;
use std::{
    fs,
    os::unix::{fs::PermissionsExt, fs::symlink},
};

#[test]
fn stateless_cursor_accepts_canonical_root_alias_and_rejects_another_root() {
    let directory = test_directory();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    let bytes = source_bytes(205);
    fs::write(root.join("src.rs"), &bytes).unwrap();
    let cache = blocked_cache(directory.path());
    let first_store = Store::unpublished(&root, &cache).unwrap();
    let cursor = page(&first_store, "path:src.rs")["next"]
        .as_str()
        .unwrap()
        .to_owned();

    let alias = directory.path().join("repo-alias");
    symlink(&root, &alias).unwrap();
    let alias_store = Store::unpublished(&alias, &cache).unwrap();
    let resumed = page(&alias_store, &cursor);
    assert_eq!(resumed["path"], "src.rs");

    let other_root = directory.path().join("other-repo");
    fs::create_dir_all(&other_root).unwrap();
    fs::write(other_root.join("src.rs"), &bytes).unwrap();
    let other_store = Store::unpublished(&other_root, &cache).unwrap();
    let error = show(&other_store, &cursor, &OutputBudget::new(20_000).unwrap()).unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("invalid_cursor: cursor belongs to a different repository root"),
        "{error:#}"
    );
}

#[test]
fn raw_spaced_path_keeps_one_canonical_display_in_saved_and_stateless_pages() {
    let directory = test_directory();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("src file.rs"), source_bytes(205)).unwrap();
    let expected_path = "src%20file.rs";

    let saved_cache = directory.path().join("saved-cache");
    let saved_store = Store::unpublished(&root, &saved_cache).unwrap();
    let saved_first = page(&saved_store, "path:src file.rs");
    assert_eq!(saved_first["path"], expected_path);
    let saved_cursor = saved_first["next"].as_str().unwrap();
    assert!(!saved_cursor.starts_with("read:path:"));
    let saved_resume_store = Store::unpublished(&root, &saved_cache).unwrap();
    assert_eq!(
        page(&saved_resume_store, saved_cursor)["path"],
        expected_path
    );

    let stateless_cache = blocked_cache(directory.path());
    let stateless_store = Store::unpublished(&root, &stateless_cache).unwrap();
    let stateless_first = page(&stateless_store, "path:src file.rs");
    assert_eq!(stateless_first["path"], expected_path);
    let stateless_cursor = stateless_first["next"].as_str().unwrap();
    assert!(stateless_cursor.starts_with(&format!(
        "read:path:{}:{expected_path}:",
        super::root_digest(&root)
    )));
    let stateless_resume_store = Store::unpublished(&root, &stateless_cache).unwrap();
    assert_eq!(
        page(&stateless_resume_store, stateless_cursor)["path"],
        expected_path
    );
}

#[test]
fn stateless_continuation_preserves_missing_and_over_limit_source_errors() {
    let directory = test_directory();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    let bytes = source_bytes(205);
    fs::write(root.join("src.rs"), &bytes).unwrap();
    let cache = blocked_cache(directory.path());
    let store = Store::unpublished(&root, &cache).unwrap();
    let cursor = page(&store, "path:src.rs")["next"]
        .as_str()
        .unwrap()
        .to_owned();

    fs::remove_file(root.join("src.rs")).unwrap();
    let missing = show(&store, &cursor, &OutputBudget::new(20_000).unwrap()).unwrap_err();
    assert!(
        missing.to_string().starts_with("source_unavailable:"),
        "{missing:#}"
    );

    fs::write(root.join("src.rs"), bytes).unwrap();
    fs::write(
        root.join("src.rs"),
        vec![b'x'; crate::source::MAX_READ_BYTES + 1],
    )
    .unwrap();
    let oversized = show(&store, &cursor, &OutputBudget::new(20_000).unwrap()).unwrap_err();
    assert!(
        oversized.to_string().starts_with("source_excluded:"),
        "{oversized:#}"
    );
}

#[test]
fn stateless_continuation_preserves_permission_refusal_and_io_cause() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let directory = test_directory();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("src.rs"), source_bytes(205)).unwrap();
    let cache = blocked_cache(directory.path());
    let store = Store::unpublished(&root, &cache).unwrap();
    let cursor = page(&store, "path:src.rs")["next"]
        .as_str()
        .unwrap()
        .to_owned();
    fs::set_permissions(root.join("src.rs"), fs::Permissions::from_mode(0)).unwrap();

    let error = show(&store, &cursor, &OutputBudget::new(20_000).unwrap()).unwrap_err();

    fs::set_permissions(root.join("src.rs"), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        error.to_string().starts_with("source_unavailable:"),
        "{error:#}"
    );
    assert!(
        error.chain().any(|cause| cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)),
        "permission cause was not retained: {error:#}"
    );
}

#[test]
fn more_with_stateless_path_cursor_points_to_show() {
    let directory = test_directory();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("src.rs"), source_bytes(205)).unwrap();
    let cache = blocked_cache(directory.path());
    let store = Store::unpublished(&root, &cache).unwrap();
    let cursor = page(&store, "path:src.rs")["next"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(cursor.starts_with("read:path:"), "{cursor}");
    let error =
        crate::results::more(&store, &cursor, 10, &OutputBudget::new(20_000).unwrap()).unwrap_err();
    let message = error.to_string();
    assert!(
        message.starts_with("invalid_cursor:") && message.contains("show"),
        "{message}"
    );
}

#[test]
fn initial_page_path_is_encoded_even_before_any_continuation() {
    let directory = test_directory();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("raw name.txt"), b"single line\n").unwrap();
    let store = Store::unpublished(&root, &directory.path().join("cache")).unwrap();
    let output = show(
        &store,
        "path:raw name.txt",
        &OutputBudget::new(20_000).unwrap(),
    )
    .unwrap();
    let page: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(page["path"], "raw%20name.txt");
    assert!(page["next"].is_null());
}
