//! Per-root, coordinator, and router Unix daemons: concurrent request workers
//! beside one maintenance thread that reconciles filesystem updates.
//! Clients reach them through `crate::daemon_transport`.

mod reconciler;
mod server;
#[cfg(test)]
mod tests;

use crate::daemon_transport::{RUNNING_PROBE_WAIT, SOCKET_NAME, socket_connect};
use crate::query_deadline::QueryDeadline;
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

pub use server::{AcceptedRequest, DaemonHandler};

/// Serves a per-root index daemon: watches `root`, reconciles on the
/// maintenance thread, and answers requests concurrently from the first moment.
/// Returns once the root is gone, on `stop`, or with a failed initial reconcile.
pub fn serve(root: &Path, cache: &Path, handler: impl DaemonHandler) -> Result<()> {
    let profile = server::ServeProfile {
        name: "root",
        watching: true,
        pool: crate::request_pool::ROOT_POOL,
        tier: server::AdmissionTier::Root,
        spool: None,
    };
    server::serve(root, cache, profile, handler)
}

/// Serves a workspace coordinator without watching or indexing a root.
pub fn serve_coordinator(root: &Path, cache: &Path, handler: impl DaemonHandler) -> Result<()> {
    let profile = server::ServeProfile {
        name: "coordinator",
        watching: false,
        pool: crate::request_pool::COORDINATOR_POOL,
        tier: server::AdmissionTier::Coordinator,
        spool: None,
    };
    server::serve(root, cache, profile, handler)
}

/// Serves the per-user router on `dir`'s socket and the `spool` directory.
pub fn serve_router(dir: &Path, spool: &Path, handler: impl DaemonHandler) -> Result<()> {
    let profile = server::ServeProfile {
        name: "router",
        watching: false,
        pool: crate::request_pool::ROUTER_POOL,
        tier: server::AdmissionTier::Router,
        spool: Some(spool),
    };
    server::serve(Path::new("/"), dir, profile, handler)
}

struct DaemonSocket {
    listener: UnixListener,
    path: PathBuf,
    inode: u64,
    _lock: File,
}

impl DaemonSocket {
    fn bind(cache: &Path) -> Result<Self> {
        fs::create_dir_all(cache).context("create daemon cache directory")?;
        fs::set_permissions(cache, fs::Permissions::from_mode(0o700))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(cache.join("daemon.lock"))?;
        lock.try_lock_exclusive()
            .context("a daemon already owns this repository")?;
        let path = cache.join(SOCKET_NAME);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_socket() => {
                match socket_connect::connect_by(&path, &QueryDeadline::after(RUNNING_PROBE_WAIT)) {
                    Ok(_) => bail!("a listener already owns the repository socket"),
                    Err(error)
                        if matches!(
                            error.kind(),
                            ErrorKind::NotFound | ErrorKind::ConnectionRefused
                        ) => {}
                    Err(error) => return Err(error).context("inspect existing repository socket"),
                }
                fs::remove_file(&path)?;
            }
            Ok(_) => bail!("refusing to remove non-socket at {}", path.display()),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let listener = UnixListener::bind(&path).context("bind repository daemon socket")?;
        let socket = Self {
            listener,
            inode: fs::symlink_metadata(&path)?.ino(),
            path,
            _lock: lock,
        };
        fs::set_permissions(&socket.path, fs::Permissions::from_mode(0o600))?;
        socket.listener.set_nonblocking(true)?;
        Ok(socket)
    }
}

impl Drop for DaemonSocket {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|metadata| metadata.file_type().is_socket() && metadata.ino() == self.inode)
        {
            let _ = fs::remove_file(&self.path);
        }
        let _ = FileExt::unlock(&self._lock);
    }
}
