//! Per-user runtime locations shared by clients, the system router, and board:
//! the router's socket directory and the spool sandboxed clients can reach.
use std::{ffi::OsString, path::PathBuf};

pub(crate) fn dir_from(
    get: impl Fn(&str) -> Option<OsString>,
    login_session: impl Fn() -> Option<PathBuf>,
) -> Option<PathBuf> {
    // Empty and relative values fall through at every step, including an
    // explicit but empty override: only absolute paths resolve.
    let absolute = |name: &str| {
        get(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    if let Some(dir) = absolute("TRUFFLEPIG_SYSTEM_DIR") {
        return Some(dir);
    }
    if let Some(base) = absolute("XDG_RUNTIME_DIR") {
        return Some(base.join("trufflepig").join("system"));
    }
    if let Some(session) = login_session() {
        return Some(session.join("trufflepig").join("system"));
    }
    let base =
        absolute("XDG_CACHE_HOME").or_else(|| absolute("HOME").map(|home| home.join(".cache")))?;
    Some(base.join("trufflepig").join("system"))
}

/// The login session's `/run/user/<uid>` when it exists, is owned by us,
/// and is mode 0700.
fn login_session_runtime_dir() -> Option<PathBuf> {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let candidate = PathBuf::from(format!("/run/user/{uid}"));
    let metadata = candidate.metadata().ok()?;
    use std::os::unix::fs::MetadataExt;
    (metadata.is_dir() && metadata.uid() == uid && metadata.mode() & 0o7777 == 0o700)
        .then_some(candidate)
}

/// Per-user runtime directory holding the system daemon socket.
pub fn system_dir() -> Option<PathBuf> {
    dir_from(|name| std::env::var_os(name), login_session_runtime_dir)
}

pub(crate) fn spool_dir_from(get: impl Fn(&str) -> Option<OsString>, uid: u32) -> PathBuf {
    match get("TRUFFLEPIG_SPOOL_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(format!("/tmp/trufflepig-{uid}/spool")),
    }
}

/// Spool directory for sandboxed clients; `/tmp` is what such sandboxes leave writable.
pub fn spool_dir() -> PathBuf {
    // SAFETY: getuid has no preconditions and cannot fail.
    spool_dir_from(|name| std::env::var_os(name), unsafe { libc::getuid() })
}

#[cfg(test)]
mod tests;
