//! Disk-backed scratch for board regressions, removed by each test's TempDir.
//! Scratch directories are owner-only because board opens refuse
//! group/world-accessible database parents they did not create.
use std::io::Read;
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

/// Reads into `buffer`, retrying interrupted syscalls: signal tests run
/// beside blocking test reads under the full suite, so a raw `read` can
/// return `ErrorKind::Interrupted`, which callers must retry.
pub(crate) fn read_ignoring_interrupts(
    stream: &mut std::net::TcpStream,
    buffer: &mut [u8],
) -> std::io::Result<usize> {
    loop {
        match stream.read(buffer) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}
