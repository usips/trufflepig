//! Last-bound port record: retaken on the next start, never removed on exit.
use anyhow::Result;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

const PORT_FILE: &str = "board-web.port";

/// Lenient read of the recorded port; absent or invalid content is ignored.
pub(super) fn read(runtime: &Path) -> Option<u16> {
    let bytes = fs::read(runtime.join(PORT_FILE)).ok()?;
    std::str::from_utf8(&bytes)
        .ok()?
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
}

/// Record the bound port for the next start: a private 0600 pending file,
/// atomically renamed over any previous or planted entry, directory synced.
pub(super) fn record(runtime: &Path, port: u16) -> Result<()> {
    fs::create_dir_all(runtime)?;
    let pending = runtime.join(format!(
        ".board-web-port-{}.pending",
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&pending)?;
        file.write_all(port.to_string().as_bytes())?;
        file.sync_all()?;
        fs::rename(&pending, runtime.join(PORT_FILE))?;
        File::open(runtime)?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(pending);
    result
}
