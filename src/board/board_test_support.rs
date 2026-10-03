//! Disk-backed scratch for board regressions, removed by each test's TempDir.
use std::path::PathBuf;

pub(crate) fn scratch(prefix: &str) -> tempfile::TempDir {
    let parent = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .expect("HOME or TMPDIR is required for disk-backed scratch");
            PathBuf::from(home).join(".cache/codex-tmp")
        });
    assert!(parent.is_absolute(), "TMPDIR must be absolute");
    assert!(
        !parent.starts_with("/tmp"),
        "board test scratch must not use tmpfs"
    );
    std::fs::create_dir_all(&parent).unwrap();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(parent)
        .unwrap()
}
