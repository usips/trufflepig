use super::*;
use serde_json::Value;
use std::{path::Path, process::Command};

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture() -> (tempfile::TempDir, Store, History) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("root");
    std::fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.name", "Test"]);
    git(&root, &["config", "user.email", "test@example.invalid"]);
    for path in ["changed.txt", "deleted.txt", "ignored.txt"] {
        std::fs::write(root.join(path), "before\r\nsecond\r\n").unwrap();
    }
    git(&root, &["add", "."]);
    git(&root, &["commit", "-qm", "initial"]);
    let mut store = Store::open(&root, &directory.path().join("live")).unwrap();
    store.index().unwrap();
    let history = History::open(&root, &directory.path().join("history")).unwrap();
    (directory, store, history)
}

#[test]
fn working_generation_captures_dirty_staged_untracked_and_coverage() {
    let (_directory, mut store, history) = fixture();
    std::fs::write(store.root.join("changed.txt"), "staged\r\nsecond\r\n").unwrap();
    git(&store.root, &["add", "changed.txt"]);
    std::fs::write(store.root.join("changed.txt"), "working\r\nsecond\r\n").unwrap();
    std::fs::write(store.root.join("untracked.txt"), "untracked").unwrap();
    std::fs::write(store.root.join(".ignore"), "ignored.txt\n").unwrap();
    std::fs::remove_file(store.root.join("deleted.txt")).unwrap();
    let response = since_uncommitted(
        &history,
        &mut store,
        &history.tip,
        None,
        &OutputBudget::new(8000).unwrap(),
    )
    .unwrap();
    let value: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(value["coverage"]["endpoint"], "published_working_tree");
    assert_eq!(
        value["generation"],
        value["coverage"]["publication"]["generation"]
    );
    let hits = value["hits"].as_array().unwrap();
    assert!(
        hits.iter()
            .any(|hit| hit["name"] == "deleted.txt" && hit["status"] == "deleted")
    );
    assert!(
        hits.iter()
            .any(|hit| hit["name"] == "ignored.txt" && hit["status"] == "ignored_or_excluded")
    );
    let after = hits
        .iter()
        .find(|hit| hit["entry"] == "live_source" && hit["path"] == "changed.txt")
        .unwrap();
    assert_eq!(
        after["revision"],
        ContentRevision::of(b"working\r\nsecond\r\n").to_string()
    );
    assert_eq!(after["start"], 0);
    assert_eq!(after["end"], 9);
    assert!(
        hits.iter()
            .any(|hit| hit["entry"] == "live_source" && hit["path"] == "untracked.txt")
    );
}

#[test]
fn working_generation_default_budget_persists_both_source_sides() {
    let (_directory, mut store, history) = fixture();
    std::fs::write(store.root.join("changed.txt"), "working\r\nsecond\r\n").unwrap();
    let budget = OutputBudget::new(600).unwrap();
    let response = since_uncommitted(
        &history,
        &mut store,
        &history.tip,
        Some("path:changed.txt"),
        &budget,
    )
    .unwrap();
    assert!(budget.fits(&response));
    let page: Value = serde_json::from_str(&response).unwrap();
    assert!(!page["hits"].as_array().unwrap().is_empty());
    let snapshot = store.published_files().unwrap();
    assert_eq!(
        page["coverage"]["publication"]["generation"],
        snapshot.publication.generation
    );
    std::fs::write(store.root.join("changed.txt"), "before\r\nsecond\r\n").unwrap();
    let response = since_uncommitted(
        &history,
        &mut store,
        &history.tip,
        Some("path:changed.txt"),
        &budget,
    )
    .unwrap();
    let reverted: Value = serde_json::from_str(&response).unwrap();
    assert!(reverted["hits"].as_array().unwrap().is_empty());
}

#[test]
fn working_generation_rejects_stale_selection_and_excludes_excessive_lines() {
    let (_directory, mut store, history) = fixture();
    std::fs::write(store.root.join("changed.txt"), "working\r\nsecond\r\n").unwrap();
    let budget = OutputBudget::new(8000).unwrap();
    let response = since_uncommitted(
        &history,
        &mut store,
        &history.tip,
        Some("path:changed.txt"),
        &budget,
    )
    .unwrap();
    let page: Value = serde_json::from_str(&response).unwrap();
    let after = page["hits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| hit["entry"] == "live_source")
        .unwrap();
    let handle = after["handle"].as_str().unwrap();
    std::fs::write(store.root.join("changed.txt"), "newer bytes\n").unwrap();
    let error =
        since_uncommitted(&history, &mut store, &history.tip, Some(handle), &budget).unwrap_err();
    assert!(error.to_string().contains("stale_handle"));
    std::fs::write(
        store.root.join("changed.txt"),
        "\n".repeat(MAX_DIFF_LINES + 1),
    )
    .unwrap();
    let response = since_uncommitted(
        &history,
        &mut store,
        &history.tip,
        Some("path:changed.txt"),
        &budget,
    )
    .unwrap();
    let page: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(page["coverage"]["excluded"], 1);
    assert_eq!(page["hits"][0]["status"], "diff_line_resource_excluded");
}
