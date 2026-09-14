use super::*;
use std::process::Command;

fn run_git(root: &Path, args: &[&str]) -> String {
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

fn repository(source: &[u8]) -> (tempfile::TempDir, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    run_git(root.path(), &["init", "-q"]);
    run_git(
        root.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    run_git(root.path(), &["config", "user.name", "Test"]);
    std::fs::write(root.path().join("a.rs"), source).unwrap();
    commit(root.path(), "root");
    (root, cache)
}

fn commit(root: &Path, message: &str) -> String {
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-qm", message]);
    run_git(root, &["rev-parse", "HEAD"])
}

fn live(root: &Path, cache: &Path) -> Store {
    let mut store = Store::open(root, &cache.join("live")).unwrap();
    store.index().unwrap();
    store
}

fn decode(response: String) -> Value {
    serde_json::from_str(&response).unwrap()
}
fn budget() -> OutputBudget {
    OutputBudget::new(8000).unwrap()
}

#[test]
fn selected_duplicate_handle_excludes_sibling_changes() {
    let original = b"fn duplicate() { first(); }\nfn duplicate() { old(); }\n";
    let (root, cache) = repository(original);
    let initial = run_git(root.path(), &["rev-parse", "HEAD"]);
    std::fs::write(
        root.path().join("a.rs"),
        b"fn duplicate() { first(); }\nfn duplicate() { new(); }\n",
    )
    .unwrap();
    let sibling_commit = commit(root.path(), "edit sibling");
    let mut history = History::open(root.path(), cache.path()).unwrap();
    history.index().unwrap();
    let mut store = live(root.path(), cache.path());
    let selection = decode(
        history
            .hist(&mut store, "sym:duplicate", &budget())
            .unwrap(),
    );
    assert_eq!(selection["coverage"]["selection_required"], true);
    assert_eq!(selection["hits"].as_array().unwrap().len(), 2);
    let first = selection["hits"][0]["handle"].as_str().unwrap();
    let result = decode(history.hist(&mut store, first, &budget()).unwrap());
    let hits = result["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["after"]["commit"], initial);
    assert_ne!(hits[0]["after"]["commit"], sibling_commit);
    assert_eq!(hits[0]["after"]["span"]["start"], 0);
    assert_eq!(
        hits[0]["after"]["span"]["end"],
        original.iter().position(|&byte| byte == b'\n').unwrap()
    );
}

#[test]
fn old_dated_rename_still_connects_eligible_history() {
    let (root, cache) = repository(b"fn a() { old(); }\n");
    let initial = run_git(root.path(), &["rev-parse", "HEAD"]);
    run_git(root.path(), &["mv", "a.rs", "b.rs"]);
    let output = Command::new("git")
        .arg("-C")
        .arg(root.path())
        .args(["commit", "-qm", "old timestamp rename"])
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .output()
        .unwrap();
    assert!(output.status.success());
    let rename = run_git(root.path(), &["rev-parse", "HEAD"]);
    std::fs::write(root.path().join("b.rs"), b"fn a() { new(); }\n").unwrap();
    let latest = commit(root.path(), "recent edit");
    let mut history = History::open(root.path(), cache.path()).unwrap();
    assert_eq!(history.index().unwrap()["visited"], 3);
    let mut store = live(root.path(), cache.path());
    let response = decode(history.hist(&mut store, "path:b.rs", &budget()).unwrap());
    let commits: Vec<_> = response["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["after"]["commit"].as_str().unwrap())
        .collect();
    assert_eq!(commits, vec![latest.as_str(), initial.as_str()]);
    assert!(!commits.contains(&rename.as_str()));
    assert_eq!(response["coverage"]["examined"], 3);
}

#[test]
fn traversal_resumes_after_first_atomic_batch() {
    let (root, cache) = repository(b"fn a() {}\n");
    let tree = run_git(root.path(), &["rev-parse", "HEAD^{tree}"]);
    let mut parent = run_git(root.path(), &["rev-parse", "HEAD"]);
    for index in 0..130 {
        parent = run_git(
            root.path(),
            &[
                "commit-tree",
                &tree,
                "-p",
                &parent,
                "-m",
                &format!("commit {index}"),
            ],
        );
    }
    run_git(root.path(), &["update-ref", "HEAD", &parent]);
    {
        let mut history = History::open(root.path(), cache.path()).unwrap();
        let status = history.index().unwrap();
        assert_eq!(status["visited"], 128);
        assert_eq!(status["complete"], false);
        assert_eq!(status["coverage"], "indexing");
    }
    let mut resumed = History::open(root.path(), cache.path()).unwrap();
    let status = resumed.index().unwrap();
    assert_eq!(status["visited"], 131);
    assert_eq!(status["complete"], true);
    assert_eq!(status["coverage"], "root");
    let unique: i64 = resumed
        .conn
        .query_row(
            "SELECT count(DISTINCT oid) FROM traversal WHERE tip=?1",
            [&parent],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unique, 131);
}

#[test]
fn since_nonancestor_uses_direct_endpoints() {
    let (root, cache) = repository(b"fn a() { base(); }\n");
    let base = run_git(root.path(), &["rev-parse", "HEAD"]);
    run_git(root.path(), &["checkout", "-qb", "side"]);
    std::fs::write(root.path().join("side.rs"), b"fn side() {}\n").unwrap();
    let side = commit(root.path(), "side addition");
    run_git(root.path(), &["checkout", "-qB", "destination", &base]);
    std::fs::write(root.path().join("a.rs"), b"fn a() { destination(); }\n").unwrap();
    let destination = commit(root.path(), "destination edit");
    let history = History::open(root.path(), cache.path()).unwrap();
    let mut store = live(root.path(), cache.path());
    let response = decode(
        history
            .since(&mut store, &side, None, false, &budget())
            .unwrap(),
    );
    assert_eq!(response["coverage"]["before"], side);
    assert_eq!(response["coverage"]["after"], destination);
    assert_eq!(response["coverage"]["comparison"], "direct_net");
    let deleted = response["hits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| hit["name"] == "side.rs")
        .unwrap();
    assert_eq!(deleted["status"], "deleted");
    assert_eq!(deleted["before"]["commit"], side);
    assert!(deleted["after"].is_null());
}

#[test]
fn scoped_diff_has_one_context_line_and_original_byte_offsets() {
    let before = "// π\r\n// before context\r\nold();\r\n// after context\r\n// last\r\n";
    let after = "// π\r\n// before context\r\nnew_longer();\r\n// after context\r\n// last\r\n";
    let (root, cache) = repository(before.as_bytes());
    std::fs::write(root.path().join("a.rs"), after.as_bytes()).unwrap();
    let tip = commit(root.path(), "change original bytes");
    let history = History::open(root.path(), cache.path()).unwrap();
    let mut store = live(root.path(), cache.path());
    let response = decode(
        history
            .diff(&mut store, &tip, Some("path:a.rs"), &budget())
            .unwrap(),
    );
    assert_eq!(response["hunks"].as_array().unwrap().len(), 1);
    let hunk = &response["hunks"][0];
    for (side, source, changed) in [
        ("before", before, "old();"),
        ("after", after, "new_longer();"),
    ] {
        let start = source.find("// before context").unwrap();
        let end = source.find("// last").unwrap();
        assert_eq!(hunk[side]["start"], start);
        assert_eq!(hunk[side]["end"], end);
        assert_eq!(
            hunk[side]["text"],
            format!("// before context\r\n{changed}\r\n// after context\r\n")
        );
        assert_eq!(response["hits"][0][side]["span"]["start"], start);
        assert_eq!(response["hits"][0][side]["span"]["end"], end);
    }
}
