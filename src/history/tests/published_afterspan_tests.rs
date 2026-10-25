//! A history live-source span stays bounded when `show` pages through it.
use super::*;
use crate::source::show;

/// One committed `lib.rs` with a 220-line body.
fn committed_fixture() -> (tempfile::TempDir, tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q"]);
    git(
        root.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    git(root.path(), &["config", "user.name", "Test"]);
    let bytes = format!(
        "fn original() {{\n{}\n}}\n",
        "    original();\n".repeat(220)
    );
    std::fs::write(root.path().join("lib.rs"), bytes).unwrap();
    git(root.path(), &["add", "lib.rs"]);
    git(
        root.path(),
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-qm",
            "original",
        ],
    );
    let store = Store::open(root.path(), cache.path()).unwrap();
    (root, cache, store)
}

#[test]
fn published_afterspan_bounds_show_and_immutable_continuation() {
    let (root, cache, mut store) = committed_fixture();
    let history_cache = tempfile::tempdir().unwrap();
    let history = History::open(root.path(), history_cache.path()).unwrap();
    let bytes = format!(
        "fn original() {{\n{}{}{}\n}}\n",
        "    original();\n".repeat(10),
        "    changed();\n".repeat(200),
        "    original();\n".repeat(10)
    );
    std::fs::write(root.path().join("lib.rs"), &bytes).unwrap();
    let response = history
        .since(
            &mut store,
            "HEAD",
            Some("path:lib.rs"),
            true,
            &OutputBudget::new(8000).unwrap(),
        )
        .unwrap();
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    let after = response["hits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| hit["entry"] == "live_source")
        .unwrap();
    assert_eq!(after["kind"], "source_region");
    assert!(after["start"].as_u64().unwrap() > 0);
    assert!(after["end"].as_u64().unwrap() < bytes.len() as u64);
    let budget = OutputBudget::new(600).unwrap();
    let first: serde_json::Value =
        serde_json::from_str(&show(&store, after["handle"].as_str().unwrap(), &budget).unwrap())
            .unwrap();
    assert_eq!(first["start"], after["start"]);
    assert_eq!(first["end"], after["end"]);
    assert!(
        first["lines"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["text"] == "    changed();\n")
    );
    let cursor = first["next"].as_str().unwrap().to_owned();
    drop(store);
    let store = Store::open(root.path(), cache.path()).unwrap();
    let second: serde_json::Value =
        serde_json::from_str(&show(&store, &cursor, &budget).unwrap()).unwrap();
    assert_eq!(
        second["start"],
        first["lines"].as_array().unwrap().last().unwrap()["end"]
    );
    assert_eq!(second["end"], after["end"]);
    assert!(
        second["lines"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["text"] == "    changed();\n")
    );
}
