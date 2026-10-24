//! Per-root, coordinator, and router Unix daemons: concurrent request workers
//! beside one maintenance thread that reconciles filesystem updates.

pub(crate) mod pool;
mod protocol;
mod reconciler;
mod server;
mod socket_connect;
pub mod spool;
#[cfg(test)]
mod tests;
mod transport_failure;

use crate::coded_error::CodedContext;
use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
#[cfg(test)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::query_deadline::QueryDeadline;
pub(crate) use pool::{PoolSize, RequestClass, RequestLane, RequestPool};
use protocol::{DaemonReply, DaemonRequest};
pub use server::{AcceptedRequest, DaemonHandler};
pub(crate) use transport_failure::{DaemonReplyTimeout, DeadlineRead, DeadlineWrite};

pub(crate) const SOCKET_NAME: &str = "daemon.sock";
/// Longest a client waits for any daemon reply, over the socket or the spool.
pub const CLIENT_REPLY_WAIT: Duration = Duration::from_secs(30);
/// Longest a daemon waits on the daemon it forwards to; shorter than
/// [`CLIENT_REPLY_WAIT`] so the client receives the forwarding failure.
pub const PROXY_REPLY_WAIT: Duration = Duration::from_secs(28);
const REQUEST_WRITE_WAIT: Duration = Duration::from_secs(5);
pub(crate) const RUNNING_PROBE_WAIT: Duration = Duration::from_millis(100);

/// Maximum encoded JSON request size, excluding its length prefix.
pub(crate) const MAX_DAEMON_REQUEST_BYTES: usize = protocol::REQUEST_LIMIT;

/// Measures the exact wire request before client-side routing or fallback.
pub(crate) fn request_encoded_size(
    args: &[String],
    context: &crate::request_context::RequestContext,
) -> Result<usize> {
    Ok(serde_json::to_vec(&arguments(args, context))?.len())
}

/// Returns `None` only when no daemon is listening; protocol errors stay errors.
pub fn request(
    cache: &Path,
    args: &[String],
    context: &crate::request_context::RequestContext,
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
    context: &crate::request_context::RequestContext,
    deadline: QueryDeadline,
) -> Result<Option<String>> {
    exchange(cache, &arguments(args, context), deadline)
}

fn arguments(args: &[String], context: &crate::request_context::RequestContext) -> DaemonRequest {
    DaemonRequest::Arguments {
        context: context.clone(),
        args: args.to_vec(),
    }
}

/// Stops a listening daemon through its authenticated-by-filesystem socket.
pub fn stop(cache: &Path) -> Result<String> {
    Ok(exchange(
        cache,
        &DaemonRequest::Stop,
        QueryDeadline::after(CLIENT_REPLY_WAIT),
    )?
    .unwrap_or_else(|| "{\"status\":\"not_running\"}".to_owned()))
}

/// Whether a daemon listens on `cache`'s socket. Its 100 ms probe never
/// contends for the startup lock; a timed-out probe conservatively reports
/// that a busy daemon may still be running.
pub fn running(cache: &Path) -> bool {
    match running_by(cache, &QueryDeadline::after(RUNNING_PROBE_WAIT)) {
        Ok(is_running) => is_running,
        Err(error) if connect_timed_out(&error) => true,
        Err(_) => false,
    }
}

fn connect_timed_out(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == ErrorKind::TimedOut)
    })
}

/// Checks for a daemon without treating an expired or cancelled connect as absence.
pub(crate) fn running_by(cache: &Path, deadline: &QueryDeadline) -> Result<bool> {
    let path = cache.join(SOCKET_NAME);
    match socket_connect::connect_by(&path, deadline) {
        Ok(_stream) => Ok(true),
        Err(error) if unreachable(error.kind()) => Ok(false),
        Err(error) if error.kind() == ErrorKind::TimedOut => {
            Err(error).with_coded_context(|| coded!(TimedOut, "phase=connecting"))
        }
        Err(error) => Err(error).context("connect to repository daemon"),
    }
}

/// Serves a per-root index daemon: watches `root`, reconciles on the
/// maintenance thread, and answers requests concurrently from the first moment.
/// Returns once the root is gone, on `stop`, or with a failed initial reconcile.
pub fn serve(root: &Path, cache: &Path, handler: impl DaemonHandler) -> Result<()> {
    let profile = server::ServeProfile {
        name: "root",
        watching: true,
        pool: pool::ROOT_POOL,
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
        pool: pool::COORDINATOR_POOL,
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
        pool: pool::ROUTER_POOL,
        tier: server::AdmissionTier::Router,
        spool: Some(spool),
    };
    server::serve(Path::new("/"), dir, profile, handler)
}

/// Sandboxed clients get EPERM/EACCES from `connect`; treat as "no daemon".
fn unreachable(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::NotFound
            | ErrorKind::NotADirectory
            | ErrorKind::ConnectionRefused
            | ErrorKind::PermissionDenied
    )
}

fn exchange(
    cache: &Path,
    request: &DaemonRequest,
    deadline: QueryDeadline,
) -> Result<Option<String>> {
    request.validate()?;
    let wait_budget = deadline.remaining();
    ensure!(
        !wait_budget.is_zero(),
        coded!(TimedOut, "phase=connecting no time left to reach a daemon")
    );
    let socket_path = cache.join(SOCKET_NAME);
    let mut stream = match socket_connect::connect_by(&socket_path, &deadline) {
        Ok(stream) => stream,
        Err(error) if unreachable(error.kind()) => {
            return Ok(None);
        }
        Err(error) if error.kind() == ErrorKind::TimedOut => {
            return Err(error).with_coded_context(|| coded!(TimedOut, "phase=connecting"));
        }
        Err(error) => return Err(error).context("connect to repository daemon"),
    };
    ensure!(
        !deadline.expired(),
        coded!(
            TimedOut,
            "phase=connecting deadline expired before request write"
        )
    );
    let write_deadline = deadline.capped(REQUEST_WRITE_WAIT);
    {
        let mut writer = DeadlineWrite::new(&mut stream, &write_deadline);
        if let Err(error) = protocol::write_request(&mut writer, request) {
            let timed_out = error.chain().any(|cause| {
                cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
                    matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
                })
            });
            if timed_out {
                return Err(error).with_coded_context(|| coded!(TimedOut, "phase=writing_request"));
            }
            return Err(error);
        }
    }
    let reply_started = Instant::now();
    let mut reader = DeadlineRead::new(&mut stream, &deadline);
    let reply = match protocol::read_reply(&mut reader) {
        Ok(reply) => reply,
        Err(error) => {
            let timed_out = error.chain().any(|cause| {
                cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
                    matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
                })
            });
            if timed_out {
                return Err(DaemonReplyTimeout::new(reply_started.elapsed(), wait_budget).into());
            }
            return Err(error);
        }
    };
    reply.into_result().map(Some)
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
