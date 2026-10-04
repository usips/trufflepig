//! Disk-backed scratch for board regressions, removed by each test's TempDir.
//! Scratch directories are owner-only because board opens refuse
//! group/world-accessible database parents they did not create.
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
    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir_in(parent).unwrap()
}

/// Installed Git `(major, minor)`, `None` when `git version` is unparsable.
pub(crate) fn git_version() -> Option<(u32, u32)> {
    let output = std::process::Command::new("git")
        .arg("version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_git_version(&output.stdout)
}

fn parse_git_version(bytes: &[u8]) -> Option<(u32, u32)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut parts = text.split_whitespace().nth(2)?.split('.');
    Some((
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn git_version_parses_dotted_release_strings() {
        assert_eq!(
            super::parse_git_version(b"git version 2.43.0\n"),
            Some((2, 43))
        );
        assert_eq!(
            super::parse_git_version(b"git version 2.55.0\n"),
            Some((2, 55))
        );
        assert_eq!(
            super::parse_git_version(b"git version 2.46.1\n"),
            Some((2, 46))
        );
        assert_eq!(super::parse_git_version(b"unexpected\n"), None);
        assert!(super::git_version().is_some());
    }
}
