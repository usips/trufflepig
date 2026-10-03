//! Per-root, coordinator, and router Unix daemons: concurrent request workers
//! beside one maintenance thread that reconciles filesystem updates.

pub mod deadline;
pub(crate) mod pool;
mod protocol;
mod reconciler;
mod server;
pub mod spool;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use deadline::QueryDeadline;
pub(crate) use pool::{PoolSize, RequestPool};
use protocol::{DaemonReply, DaemonRequest};
pub use server::{AcceptedRequest, DAEMON_BUSY, DaemonHandler};

const SOCKET_NAME: &str = "daemon.sock";
/// Longest a client waits for any daemon reply, over the socket or the spool.
pub const CLIENT_REPLY_WAIT: Duration = Duration::from_secs(30);
/// Longest a daemon waits on the daemon it forwards to; shorter than
/// [`CLIENT_REPLY_WAIT`] so the client receives the forwarding failure.
pub const PROXY_REPLY_WAIT: Duration = Duration::from_secs(28);

/// Maximum encoded JSON request size, excluding its length prefix.
pub(crate) const MAX_DAEMON_REQUEST_BYTES: usize = protocol::REQUEST_LIMIT;

/// Measures the exact wire request before client-side routing or fallback.
pub(crate) fn request_encoded_size(
    args: &[String],
    context: &crate::diagnostics::RequestContext,
) -> Result<usize> {
    Ok(serde_json::to_vec(&arguments(args, context))?.len())
}

/// Returns `None` only when no daemon is listening; protocol errors stay errors.
pub fn request(
    cache: &Path,
    args: &[String],
    context: &crate::diagnostics::RequestContext,
) -> Result<Option<String>> {
    request_by(
        cache,
        args,
        context,
        QueryDeadline::after(CLIENT_REPLY_WAIT),
    )
}

/// [`request`] waiting for the reply only until `deadline`, so a daemon that
/// forwards never waits longer than the client in front of it.
pub fn request_by(
    cache: &Path,
    args: &[String],
    context: &crate::diagnostics::RequestContext,
    deadline: QueryDeadline,
) -> Result<Option<String>> {
    exchange(cache, &arguments(args, context), deadline.remaining())
}

fn arguments(args: &[String], context: &crate::diagnostics::RequestContext) -> DaemonRequest {
    DaemonRequest::Arguments {
        context: context.clone(),
        args: args.to_vec(),
    }
}

/// Stops a listening daemon through its authenticated-by-filesystem socket.
pub fn stop(cache: &Path) -> Result<String> {
    Ok(exchange(cache, &DaemonRequest::Stop, CLIENT_REPLY_WAIT)?
        .unwrap_or_else(|| "{\"status\":\"not_running\"}".to_owned()))
}

/// Whether a daemon listens on `cache`'s socket. A connect probe never
/// contends for the startup lock, so it cannot make a starting daemon exit;
/// the daemon drops the connection unanswered.
pub fn running(cache: &Path) -> bool {
    UnixStream::connect(cache.join(SOCKET_NAME)).is_ok()
}

/// Whether a daemon spawned for `cache` is gone with no other owner: a racing
/// spawn exits on the startup lock while the winner still starts serving.
pub(crate) fn spawn_failed(
    child: &crate::background_process::BackgroundChild,
    cache: &Path,
) -> bool {
    child.exited() && !running(cache)
}

/// Serves a per-root index daemon: watches `root`, reconciles on the
/// maintenance thread, and answers requests concurrently from the first moment.
/// Returns once the root is gone, on `stop`, or with a failed initial reconcile.
pub fn serve(root: &Path, cache: &Path, handler: impl DaemonHandler) -> Result<()> {
    let profile = server::ServeProfile {
        name: "root",
        watching: true,
        pool: pool::ROOT_POOL,
        spool: None,
    };
    server::serve(root, cache, profile, handler)
}

/// Serves a workspace coordinator without watching or indexing a root.
pub fn serve_coordinator(root: &Path, cache: &Path, handler: impl DaemonHandler) -> Result<()> {
    let profile = server::ServeProfile {
        name: "coordinator",
        watching: false,
        pool: pool::COORDINATOR_POOL,
        spool: None,
    };
    server::serve(root, cache, profile, handler)
}

/// Serves the per-user router on `dir`'s socket and the `spool` directory.
pub fn serve_router(dir: &Path, spool: &Path, handler: impl DaemonHandler) -> Result<()> {
    let profile = server::ServeProfile {
        name: "router",
        watching: false,
        pool: pool::ROUTER_POOL,
        spool: Some(spool),
    };
    server::serve(Path::new("/"), dir, profile, handler)
}

/// Sandboxed clients get EPERM/EACCES from `connect`; treat as "no daemon".
fn unreachable(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::NotFound | ErrorKind::ConnectionRefused | ErrorKind::PermissionDenied
    )
}

fn exchange(cache: &Path, request: &DaemonRequest, wait: Duration) -> Result<Option<String>> {
    request.validate()?;
    ensure!(
        !wait.is_zero(),
        "{}: no time left to wait for a daemon reply",
        deadline::TIMED_OUT
    );
    let mut stream = match UnixStream::connect(cache.join(SOCKET_NAME)) {
        Ok(stream) => stream,
        Err(error) if unreachable(error.kind()) => {
            return Ok(None);
        }
        Err(error) => return Err(error).context("connect to repository daemon"),
    };
    stream.set_read_timeout(Some(wait))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    protocol::write_request(&mut stream, request)?;
    match protocol::read_reply(&mut stream)? {
        DaemonReply::Success { output } => Ok(Some(output)),
        DaemonReply::Failure { message } => bail!("daemon: {message}"),
    }
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
                match UnixStream::connect(&path) {
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
