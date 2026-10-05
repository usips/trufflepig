//! Probe unit tests over the logic shared with `build.rs`.
include!("../build/tool_probe.rs");

use std::{fs, path::PathBuf};

/// Write `name` into `dir` with Unix `mode` bits; return the file path.
#[cfg(unix)]
fn write_mode_file(dir: &std::path::Path, name: &str, mode: u32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write probe fixture");
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("chmod fixture");
    path
}

#[test]
#[cfg(unix)]
fn resolver_finds_executable_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let expected = write_mode_file(tmp.path(), "git", 0o755);
    let found = tool_probe::resolve_tool_in_dirs(&[tmp.path().to_owned()], "git");
    assert_eq!(found, Some(expected));
}

#[test]
#[cfg(unix)]
fn resolver_skips_non_executable_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_mode_file(tmp.path(), "git", 0o644);
    let found = tool_probe::resolve_tool_in_dirs(&[tmp.path().to_owned()], "git");
    assert_eq!(
        found, None,
        "non-executable file must not satisfy the probe"
    );
}

#[test]
#[cfg(unix)]
fn resolver_prefers_first_executable_on_path() {
    let first = tempfile::tempdir().expect("tempdir");
    let second = tempfile::tempdir().expect("tempdir");
    write_mode_file(first.path(), "git", 0o644);
    let expected = write_mode_file(second.path(), "git", 0o755);
    let dirs = vec![first.path().to_owned(), second.path().to_owned()];
    let found = tool_probe::resolve_tool_in_dirs(&dirs, "git");
    assert_eq!(found, Some(expected));
}

#[test]
fn resolver_searches_dirs_in_order_with_injected_predicate() {
    let dirs = vec![PathBuf::from("/fake/a"), PathBuf::from("/fake/b")];
    let found =
        tool_probe::resolve_tool_with(&dirs, "git", |path| path == PathBuf::from("/fake/b/git"));
    assert_eq!(found, Some(PathBuf::from("/fake/b/git")));
    let missing = tool_probe::resolve_tool_with(&dirs, "git", |_| false);
    assert_eq!(missing, None);
}

#[test]
fn resolver_rejects_directories_and_missing_files() {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::create_dir(tmp.path().join("git")).expect("mkdir fixture");
    let found = tool_probe::resolve_tool_in_dirs(&[tmp.path().to_owned()], "git");
    assert_eq!(found, None, "a directory must not satisfy the probe");
    assert!(!tool_probe::is_executable_file(&tmp.path().join("absent")));
}

#[test]
fn missing_tool_watches_each_path_dir() {
    let joined = std::env::join_paths(["/a", "/b", "/c"]).expect("join PATH");
    let dirs = tool_probe::path_search_dirs(Some(&joined));
    assert_eq!(
        dirs,
        vec![
            PathBuf::from("/a"),
            PathBuf::from("/b"),
            PathBuf::from("/c")
        ]
    );
}

#[test]
fn path_search_skips_empty_entries_and_unset_path() {
    let joined = std::env::join_paths(["/a", "", "/b"]).expect("join PATH");
    let dirs = tool_probe::path_search_dirs(Some(&joined));
    assert_eq!(dirs, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    assert!(tool_probe::path_search_dirs(None).is_empty());
}

#[test]
fn probe_report_writer_roundtrips() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let report = tool_probe::format_probe_report(&[("git", "missing (searched 3 PATH dirs)")]);
    let path = tool_probe::write_probe_report(tmp.path(), &report).expect("write report");
    assert_eq!(path, tmp.path().join(tool_probe::REPORT_FILENAME));
    assert_eq!(fs::read_to_string(&path).expect("read report"), report);
    assert_eq!(report, "git=missing (searched 3 PATH dirs)\n");
}

#[test]
fn probe_version_parsers_accept_and_reject() {
    assert_eq!(
        tool_probe::parse_git_version(b"git version 2.55.0\n"),
        Some((2, 55))
    );
    assert_eq!(tool_probe::parse_git_version(b"garbage"), None);
    assert_eq!(tool_probe::parse_node_version(b"v26.10.0\n"), Some(26));
    assert_eq!(tool_probe::parse_node_version(b"garbage"), None);
}
