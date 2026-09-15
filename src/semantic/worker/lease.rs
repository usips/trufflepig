use anyhow::{Context, Result, bail};
use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    io::ErrorKind,
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
};

pub const SOCKET_NAME: &str = "worker.sock";
const WORKER_LOCK: &str = "worker.lock";
const DEVICE_LOCK: &str = "device-model.lock";

pub fn worker_cache_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .context("semantic_worker_unavailable: set HOME or XDG_CACHE_HOME")?;
    Ok(base.join("trufflepig").join("inference-worker"))
}

fn private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

pub struct WorkerLease {
    worker: File,
    device: File,
}

impl WorkerLease {
    pub fn acquire(cache: &Path) -> Result<Self> {
        Self::acquire_with_device(cache, cache)
    }

    /// Acquire a socket lease while always taking the per-user device lease.
    pub fn acquire_with_device(cache: &Path, device_cache: &Path) -> Result<Self> {
        private_directory(cache)?;
        private_directory(device_cache)?;
        let worker = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(cache.join(WORKER_LOCK))?;
        let device = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(device_cache.join(DEVICE_LOCK))?;
        let lease = Self { worker, device };
        if let Err(error) = lease.worker.try_lock_exclusive() {
            if error.kind() == ErrorKind::WouldBlock {
                bail!("semantic_busy: inference worker owns the device/model lease");
            }
            return Err(error).context("semantic_worker: acquire worker lease");
        }
        if let Err(error) = lease.device.try_lock_exclusive() {
            let _ = FileExt::unlock(&lease.worker);
            if error.kind() == ErrorKind::WouldBlock {
                bail!("semantic_busy: another process owns the device/model lease");
            }
            return Err(error).context("semantic_worker: acquire device/model lease");
        }
        Ok(lease)
    }
}

impl Drop for WorkerLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.device);
        let _ = FileExt::unlock(&self.worker);
    }
}

pub struct WorkerSocket {
    listener: UnixListener,
    path: PathBuf,
    inode: u64,
    _lease: WorkerLease,
}

impl WorkerSocket {
    pub fn bind(cache: &Path) -> Result<Self> {
        let device_cache = worker_cache_dir()?;
        Self::bind_with_device(cache, &device_cache)
    }

    fn bind_with_device(cache: &Path, device_cache: &Path) -> Result<Self> {
        private_directory(cache)?;
        let lease = WorkerLease::acquire_with_device(cache, device_cache)?;
        let path = cache.join(SOCKET_NAME);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_socket() => match UnixStream::connect(&path) {
                Ok(_) => bail!("semantic_worker_busy: worker socket already active"),
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::NotFound | ErrorKind::ConnectionRefused
                    ) =>
                {
                    fs::remove_file(&path)?
                }
                Err(error) => return Err(error).context("inspect semantic worker socket"),
            },
            Ok(_) => bail!(
                "semantic_worker: refusing to remove non-socket at {}",
                path.display()
            ),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let listener = UnixListener::bind(&path).context("bind semantic worker socket")?;
        let inode = std::os::unix::fs::MetadataExt::ino(&fs::symlink_metadata(&path)?);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            path,
            inode,
            _lease: lease,
        })
    }

    pub fn accept(&self) -> std::io::Result<(UnixStream, std::os::unix::net::SocketAddr)> {
        self.listener.accept()
    }
}

impl Drop for WorkerSocket {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
            metadata.file_type().is_socket()
                && std::os::unix::fs::MetadataExt::ino(&metadata) == self.inode
        }) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_user_directory_is_private_and_socket_is_removed() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let device = tempfile::tempdir()?;
        let socket = WorkerSocket::bind_with_device(directory.path(), device.path())?;
        assert_eq!(
            fs::metadata(directory.path())?.permissions().mode() & 0o777,
            0o700
        );
        drop(socket);
        assert!(!directory.path().join(SOCKET_NAME).exists());
        Ok(())
    }

    #[test]
    fn duplicate_device_lease_is_rejected() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let first = WorkerLease::acquire(directory.path())?;
        assert!(WorkerLease::acquire(directory.path()).is_err());
        drop(first);
        assert!(WorkerLease::acquire(directory.path()).is_ok());
        Ok(())
    }
}
