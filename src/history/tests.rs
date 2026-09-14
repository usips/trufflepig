use super::*;
use std::process::Command;

fn git(root: &Path, args: &[&str]) -> String {
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
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q"]);
    git(
        root.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    git(root.path(), &["config", "user.name", "Test"]);
    std::fs::write(root.path().join("a.rs"), "fn a() { 1; }\n").unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "root"]);
    (root, cache)
}

#[test]
fn captures_views_and_follows_unique_blob_rename() {
    let (root, cache) = fixture();
    let first = git(root.path(), &["rev-parse", "HEAD"]);
    git(root.path(), &["mv", "a.rs", "b.rs"]);
    git(root.path(), &["commit", "-qm", "rename"]);
    let mut history = History::open(root.path(), cache.path()).unwrap();
    let status = history.index().unwrap();
    assert_eq!(status["visited"], 2);
    assert_eq!(status["coverage"], "root");
    let changes = comparison::compare(
        &history,
        Some(&GitOid::parse(&first).unwrap()),
        &history.tip,
    )
    .unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].status, "renamed_exact_blob");
    let mut store = Store::open(root.path(), &cache.path().join("live")).unwrap();
    store.index().unwrap();
    let result: Value = serde_json::from_str(
        &history
            .hist(&mut store, "path:b.rs", &OutputBudget::new(4000).unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result["hits"].as_array().unwrap().len(), 2);
    git(root.path(), &["reset", "--hard", &first]);
    let mut second = History::open(root.path(), cache.path()).unwrap();
    second.index().unwrap();
    assert_ne!(history.tip, second.tip);
    assert_eq!(history.status().unwrap()["visited"], 2);
}

#[test]
fn subtree_diff_confines_blobs_and_net_comparison() {
    let (root, cache) = fixture();
    std::fs::create_dir(root.path().join("sub")).unwrap();
    std::fs::write(root.path().join("sub/in.rs"), "fn inside() {}\n").unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "subtree"]);
    let before = git(root.path(), &["rev-parse", "HEAD"]);
    std::fs::write(root.path().join("sub/in.rs"), "fn inside() { 2; }\n").unwrap();
    std::fs::write(root.path().join("a.rs"), "outside changed\n").unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "both"]);
    let history = History::open(&root.path().join("sub"), cache.path()).unwrap();
    let changes = comparison::compare(
        &history,
        Some(&GitOid::parse(&before).unwrap()),
        &history.tip,
    )
    .unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].after.as_ref().unwrap().path, "in.rs");
}

#[test]
fn duplicate_symbols_require_selection_and_blame_has_runs() {
    let (root, cache) = fixture();
    std::fs::write(root.path().join("other.rs"), "fn a() { 2; }\n").unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "duplicate"]);
    let mut history = History::open(root.path(), cache.path()).unwrap();
    history.index().unwrap();
    let mut store = Store::open(root.path(), &cache.path().join("live")).unwrap();
    store.index().unwrap();
    let budget = OutputBudget::new(4000).unwrap();
    let value: Value =
        serde_json::from_str(&history.hist(&mut store, "sym:a", &budget).unwrap()).unwrap();
    assert_eq!(value["coverage"]["selection_required"], true);
    assert_eq!(value["hits"].as_array().unwrap().len(), 2);
    let blame: Value = serde_json::from_str(
        &history
            .blame(&mut store, "path:a.rs", false, None, &budget)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(blame["runs"][0]["lines"], 1);
}

#[test]
fn diff_marks_omitted_hunks_as_truncated() {
    use std::fmt::Write;
    let (root, cache) = fixture();
    let mut before = String::with_capacity(400_000);
    let mut after = String::with_capacity(400_000);
    for i in 0..10_001 {
        writeln!(before, "anchor_{i}\nbefore_{i}").unwrap();
        writeln!(after, "anchor_{i}\nafter_{i}").unwrap();
    }
    std::fs::write(root.path().join("many.txt"), before).unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "before"]);
    std::fs::write(root.path().join("many.txt"), after).unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "after"]);
    let history = History::open(root.path(), cache.path()).unwrap();
    let mut store = Store::open(root.path(), &cache.path().join("live")).unwrap();
    let response: Value = serde_json::from_str(
        &history
            .diff(
                &mut store,
                "HEAD",
                Some("path:many.txt"),
                &OutputBudget::new(2000).unwrap(),
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(response["truncated"], true);
    let handle = response["hits"][0]["handle"].as_str().unwrap();
    let set = crate::results::load_entries(&store, handle.split(':').next().unwrap()).unwrap();
    assert!(!set.hits.is_empty() && set.hits.len() <= crate::results::MAX_HITS);
    assert!(set.truncated);
}

#[test]
fn source_region_handles_keep_range_without_symbol_inference() {
    let (root, cache) = fixture();
    let mut store = Store::open(root.path(), &cache.path().join("live")).unwrap();
    store.index().unwrap();
    let mut hit = crate::search::map(&store, "").unwrap().hits.remove(0);
    hit.kind = "source_region".into();
    hit.name = hit.path.clone();
    hit.start = 0;
    hit.end = 5;
    let id = crate::results::save_entries(
        &store,
        store.generation().unwrap(),
        serde_json::json!({}),
        vec![crate::results::ResultEntry::LiveSource(hit)],
        false,
    )
    .unwrap();
    let targets::Selection::Target(target) = targets::resolve(&store, &format!("{id}:1")).unwrap()
    else {
        panic!("explicit region selection");
    };
    assert!(target.symbol.is_none());
    assert_eq!(
        target.span,
        Some(crate::identity::ByteSpan { start: 0, end: 5 })
    );
}
