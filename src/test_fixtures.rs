//! Disk-backed scratch and Git repositories shared by unit tests in every layer.
//! Scratch directories are owner-only because board opens refuse
//! group/world-accessible database parents they did not create.
mod git_fixture;
pub(crate) mod git_version;
#[cfg(target_os = "linux")]
pub(crate) mod unlinked_executable;

pub(crate) use git_fixture::GitFixture;
use std::ffi::OsString;
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
        "test scratch must not use tmpfs"
    );
    std::fs::create_dir_all(&parent).unwrap();
    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir_in(parent).unwrap()
}

/// An environment lookup over fixed `(name, value)` pairs.
pub(crate) fn env_lookup<'a>(
    pairs: &'a [(&'a str, &'a str)],
) -> impl Fn(&str) -> Option<OsString> + 'a {
    move |name| {
        pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| OsString::from(value))
    }
}
