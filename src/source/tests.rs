use super::*;
use crate::{
    identity::{ByteSpan, GitOid},
    results::{self, ChangeEntry, HistoricalSource, ResultEntry},
};
use std::process::Command;

#[test]
fn rejects_traversal_and_symlink_components() {
    let temp = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink("/etc", temp.path().join("outside")).unwrap();
    assert!(read_contained(temp.path(), Path::new("outside/passwd"), 4096).is_err());
    assert!(read_contained(temp.path(), Path::new("../x"), 4096).is_err());
}

#[test]
fn original_bom_crlf_and_invalid_bytes() {
    let bytes = b"\xef\xbb\xbffirst\r\n\xffsecond\r\n";
    assert_eq!(current_span(bytes, 2, 2).unwrap(), (10, 19));
    assert_eq!(line_span(bytes, 10, 19), (2, 2));
    assert_eq!(display_bytes(&bytes[10..]).1, "byte-escaped");
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
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

fn historical_fixture(
    format: &str,
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    Store,
    HistoricalSource,
) {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &["init", "-q", &format!("--object-format={format}")],
    );
    let bytes = format!(
        "fn original() {{\n{}\n}}\n",
        "    original();\n".repeat(220)
    );
    std::fs::write(root.path().join("lib.rs"), &bytes).unwrap();
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
    let commit = GitOid::parse(&git(root.path(), &["rev-parse", "HEAD"])).unwrap();
    let blob = GitOid::parse(&git(root.path(), &["rev-parse", "HEAD:lib.rs"])).unwrap();
    let identity = HistoricalSource {
        repository: crate::store::encode_path(&root.path().join(".git").canonicalize().unwrap()),
        commit,
        blob,
        revision: crate::identity::ContentRevision::of(bytes.as_bytes()),
        path: "lib.rs".into(),
        span: ByteSpan::new(0, bytes.len()).unwrap(),
    };
    let store = Store::open(root.path(), cache.path()).unwrap();
    (root, cache, store, identity)
}

fn save_change(store: &Store, identity: HistoricalSource) -> String {
    let change = ChangeEntry {
        handle: String::new(),
        name: "original".into(),
        status: "added".into(),
        before: None,
        after: Some(identity),
        correspondence: "exact".into(),
    };
    let id = results::save_entries(
        store,
        0,
        serde_json::json!({}),
        vec![ResultEntry::Change(change)],
        false,
    )
    .unwrap();
    format!("{id}:1")
}

#[test]
fn historical_reads_and_continuations_verify_sha1_and_sha256() {
    for format in ["sha1", "sha256"] {
        let (root, cache, store, identity) = historical_fixture(format);
        let handle = save_change(&store, identity.clone());
        let budget = OutputBudget::new(600).unwrap();
        let first: serde_json::Value = serde_json::from_str(
            &show_with_side(&store, &handle, Some(SourceSide::After), &budget).unwrap(),
        )
        .unwrap();
        assert_eq!(first["historical"]["blob"], identity.blob.to_string());
        let cursor = first["next"].as_str().unwrap().to_owned();
        assert!(cursor.contains(":after@"));
        std::fs::write(root.path().join("lib.rs"), "fn replacement() {}\n").unwrap();
        drop(store);
        let store = Store::open(root.path(), cache.path()).unwrap();
        let next: serde_json::Value =
            serde_json::from_str(&show(&store, &cursor, &budget).unwrap()).unwrap();
        assert_eq!(next["historical"], first["historical"]);
        assert!(
            next["lines"][0]["text"]
                .as_str()
                .unwrap()
                .contains("original")
        );
        assert!(
            show_with_side(&store, &cursor, Some(SourceSide::Before), &budget)
                .unwrap_err()
                .to_string()
                .contains("invalid_side")
        );
        let oid = identity.blob.as_str();
        std::fs::remove_file(
            root.path()
                .join(".git/objects")
                .join(&oid[..2])
                .join(&oid[2..]),
        )
        .unwrap();
        assert!(
            show(&store, &cursor, &budget)
                .unwrap_err()
                .to_string()
                .contains("source_unavailable")
        );
    }
}

#[test]
fn historical_scope_and_regular_file_contract_are_checked() {
    let (root, cache, store, mut identity) = historical_fixture("sha1");
    identity.path = "../lib.rs".into();
    let handle = save_change(&store, identity.clone());
    let budget = OutputBudget::new(600).unwrap();
    assert!(
        show_with_side(&store, &handle, Some(SourceSide::After), &budget)
            .unwrap_err()
            .to_string()
            .contains("scope_boundary")
    );
    std::fs::create_dir(root.path().join("subtree")).unwrap();
    let subtree_cache = tempfile::tempdir().unwrap();
    let subtree = Store::open(&root.path().join("subtree"), subtree_cache.path()).unwrap();
    identity.path = "lib.rs".into();
    let handle = save_change(&subtree, identity);
    assert!(show_with_side(&subtree, &handle, Some(SourceSide::After), &budget).is_err());
    drop(cache);
}

#[test]
fn historical_reads_decode_repository_and_non_utf8_file_paths() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let root = tempfile::Builder::new()
        .prefix(OsStr::from_bytes(b"history repo % : @ \xfe "))
        .tempdir()
        .unwrap();
    let cache = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q"]);
    let filename = Path::new(OsStr::from_bytes(b"literal [x] % : @ \xff.rs"));
    let bytes = b"fn original_bytes() {}\n";
    std::fs::write(root.path().join(filename), bytes).unwrap();
    git(root.path(), &["add", "--all"]);
    git(
        root.path(),
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-qm",
            "byte filename",
        ],
    );
    let output = Command::new("git")
        .current_dir(root.path())
        .arg("hash-object")
        .arg(filename)
        .output()
        .unwrap();
    assert!(output.status.success());
    let identity = HistoricalSource {
        repository: crate::store::encode_path(&root.path().join(".git").canonicalize().unwrap()),
        commit: GitOid::parse(&git(root.path(), &["rev-parse", "HEAD"])).unwrap(),
        blob: GitOid::parse(std::str::from_utf8(&output.stdout).unwrap().trim()).unwrap(),
        revision: crate::identity::ContentRevision::of(bytes),
        path: crate::store::encode_path(filename),
        span: ByteSpan::new(0, bytes.len()).unwrap(),
    };
    assert!(identity.repository.contains("%25"));
    assert!(identity.repository.contains("%3A"));
    assert!(identity.repository.contains("%40"));
    let store = Store::open(root.path(), cache.path()).unwrap();
    let handle = save_change(&store, identity.clone());
    let value: serde_json::Value = serde_json::from_str(
        &show_with_side(
            &store,
            &handle,
            Some(SourceSide::After),
            &OutputBudget::new(600).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(value["historical"]["repository"], identity.repository);
    assert_eq!(value["path"], identity.path);
    assert_eq!(value["lines"][0]["text"], "fn original_bytes() {}\n");
}

#[test]
fn historical_content_revision_must_match_verified_blob() {
    let (_root, _cache, store, mut identity) = historical_fixture("sha1");
    identity.revision = crate::identity::ContentRevision::of(b"incorrect identity");
    let handle = save_change(&store, identity);
    let error = show_with_side(
        &store,
        &handle,
        Some(SourceSide::After),
        &OutputBudget::new(600).unwrap(),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("historical content revision does not match")
    );
}
