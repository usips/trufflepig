//! Last-bound port record: retaken on the next start, never removed on exit.
use anyhow::Result;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

const PORT_FILE: &str = "board-web.port";
const PORT_LIMIT: u64 = 16;

/// Lenient read of the recorded port through a private non-blocking open
/// capped at 16 bytes; an absent, unsafe, or invalid file is ignored.
pub(super) fn read(runtime: &Path) -> Option<u16> {
    let mut file = super::open_private(&runtime.join(PORT_FILE)).ok()?;
    let mut bytes = Vec::with_capacity(PORT_LIMIT as usize);
    Read::by_ref(&mut file)
        .take(PORT_LIMIT)
        .read_to_end(&mut bytes)
        .ok()?;
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
