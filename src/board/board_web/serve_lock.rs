//! Single-instance guard for the foreground board web service.
#[cfg(test)]
mod tests;

use anyhow::{Context, Result};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

const LOCK_FILE: &str = "board-web.lock";

/// Held for the process lifetime; dropping releases the single-instance lock.
pub(super) struct ServeLock {
    _file: File,
}

/// Take the per-user board-serve lock before rotating the token or binding; a
/// second instance fails with the live listener's origin and touches nothing.
pub(super) fn acquire_at(runtime: &Path) -> Result<ServeLock> {
    fs::create_dir_all(runtime).context("board_serve_lock: create runtime directory")?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(runtime.join(LOCK_FILE))
        .context("board_serve_lock: open lock file")?;
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(ServeLock { _file: file }),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
            Err(already_running(runtime))
        }
        Err(error) => Err(error).context("board_serve_lock: take lock"),
    }
}

fn already_running(runtime: &Path) -> anyhow::Error {
    match super::web_endpoint::published_origin(runtime) {
        Some(origin) => anyhow::anyhow!("board-serve already running at {origin}"),
        None => anyhow::anyhow!("board-serve already running"),
    }
}
