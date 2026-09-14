use super::*;
use crate::identity::ByteSpan;
use crate::results::{self, ChangeEntry, HistoricalSource, Hit, ResultEntry};
use std::process::Command;

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .trim_end()
        .to_owned()
}

fn fixture() -> (tempfile::TempDir, tempfile::TempDir, Store, History) {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q"]);
    git(root.path(), &["config", "user.name", "Test"]);
    git(
        root.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    std::fs::write(
        root.path().join("a.rs"),
        "fn original() {}\nfn second() {}\n",
    )
    .unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "initial"]);
    let mut store = Store::open(root.path(), &cache.path().join("live")).unwrap();
    store.index().unwrap();
    let history = History::open(root.path(), &cache.path().join("history")).unwrap();
    (root, cache, store, history)
}

fn blame_value(history: &History, store: &mut Store, target: &str) -> Value {
    serde_json::from_str(
        &history
            .blame(
                store,
                target,
                false,
                None,
                &OutputBudget::new(4000).unwrap(),
            )
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn blame_published_dirty_and_staged_buffers_preserve_line_coordinates() {
    let (root, _cache, mut store, history) = fixture();
    let contents = "// inserted\nfn original() {}\nfn second() {}\n";
    std::fs::write(root.path().join("a.rs"), contents).unwrap();
    git(root.path(), &["add", "a.rs"]);
    store.index().unwrap();
    let value = blame_value(&history, &mut store, "sym:second");
    assert_eq!(value["endpoint"], "published_working_tree");
    assert_eq!(value["runs"][0]["start_line"], 3);
    assert_eq!(value["runs"][0]["original_start"], 2);
    assert_eq!(value["runs"][0]["commit"], history.tip.as_str());
    std::fs::write(root.path().join("a.rs"), "unpublished filesystem edit\n").unwrap();
    let value = blame_value(&history, &mut store, "path:a.rs");
    assert_eq!(
        value["revision"],
        blake3::hash(contents.as_bytes()).to_hex().as_str()
    );
    assert_eq!(value["runs"][0]["commit"], "0".repeat(40));
    assert_eq!(value["runs"][1]["lines"], 2);
}

#[test]
fn blame_rejects_stale_live_handles_and_moved_head() {
    let (root, _cache, mut store, history) = fixture();
    let bytes = std::fs::read(root.path().join("a.rs")).unwrap();
    let hit = Hit {
        handle: String::new(),
        path: "a.rs".into(),
        revision: Some(blake3::hash(&bytes).to_hex().to_string()),
        start: 0,
        end: 16,
        start_line: 1,
        end_line: 1,
        name: "original".into(),
        kind: "function".into(),
        container: None,
        provenance: None,
        resolution: None,
        candidates: Vec::new(),
        target: None,
    };
    let id = results::save_entries(
        &store,
        store.generation().unwrap(),
        json!({}),
        vec![ResultEntry::LiveSource(hit)],
        false,
    )
    .unwrap();
    std::fs::write(
        root.path().join("a.rs"),
        "// prefix\nfn original() {}\nfn second() {}\n",
    )
    .unwrap();
    store.index().unwrap();
    let error = history
        .blame(
            &mut store,
            &format!("{id}:1"),
            false,
            None,
            &OutputBudget::new(4000).unwrap(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("stale_source"));
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "moved HEAD"]);
    let error = history
        .blame(
            &mut store,
            "path:a.rs",
            false,
            None,
            &OutputBudget::new(4000).unwrap(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("history_changed"));
}

#[test]
fn blame_historical_target_uses_its_own_commit_and_span() {
    let (root, _cache, mut store, history) = fixture();
    let commit = history.tip;
    let blob = GitOid::parse(&git(root.path(), &["rev-parse", "HEAD:a.rs"])).unwrap();
    let entry = ResultEntry::Change(ChangeEntry {
        handle: String::new(),
        name: "second".into(),
        status: "modified".into(),
        before: None,
        after: Some(HistoricalSource {
            repository: crate::store::encode_path(&history.repository.common_dir),
            commit,
            blob,
            path: "a.rs".into(),
            span: ByteSpan::new(17, 31).unwrap(),
        }),
        correspondence: "path".into(),
    });
    let id = results::save_entries(
        &store,
        store.generation().unwrap(),
        json!({}),
        vec![entry],
        false,
    )
    .unwrap();
    std::fs::write(root.path().join("a.rs"), "fn unrelated() {}\n").unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "replace"]);
    store.index().unwrap();
    let value = blame_value(&history, &mut store, &format!("{id}:1"));
    assert_eq!(value["endpoint"], "historical_commit");
    assert_eq!(value["commit"], commit.as_str());
    assert_eq!(value["blob"], blob.as_str());
    assert_eq!(value["runs"][0]["start_line"], 2);
    assert_eq!(value["runs"][0]["lines"], 1);
}

#[test]
fn blame_untracked_and_corrupt_publications_are_unavailable() {
    let (root, _cache, mut store, history) = fixture();
    std::fs::write(root.path().join("new.rs"), "fn untracked() {}\n").unwrap();
    store.index().unwrap();
    let error = history
        .blame(
            &mut store,
            "path:new.rs",
            false,
            None,
            &OutputBudget::new(4000).unwrap(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("untracked path"));
    store.conn.execute("UPDATE contents SET bytes=X'00' WHERE revision=(SELECT revision FROM files WHERE path='a.rs')", []).unwrap();
    let error = history
        .blame(
            &mut store,
            "path:a.rs",
            false,
            None,
            &OutputBudget::new(4000).unwrap(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("identity mismatch"));
}
