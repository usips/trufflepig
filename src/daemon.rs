//! Per-root Unix daemon with serialized requests and reconciled filesystem updates.

mod protocol;
pub mod spool;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use notify::{EventKind, RecursiveMode, Watcher};
use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use protocol::{DaemonReply, DaemonRequest};

const SOCKET_NAME: &str = "daemon.sock";
/// Periodic recovery reconcile without a watcher (polling mode).
const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);
/// Periodic recovery reconcile while a watcher delivers change hints. Walking a
/// large unchanged tree every 30 s kept idle daemons at a fifth of a core each.
const WATCHED_RECONCILE_INTERVAL: Duration = Duration::from_secs(300);
/// Build and VCS directories the index walk prunes; their churn is not a change.
const UNINDEXED_DIRECTORIES: [&str; 4] = [".git", "target", "node_modules", ".trufflepig"];
const DEBOUNCE: Duration = Duration::from_millis(75);
const MAX_DEBOUNCE: Duration = Duration::from_secs(1);

/// Returns `None` only when no daemon is listening; protocol errors stay errors.
pub fn request(
    cache: &Path,
    args: &[String],
    context: &crate::diagnostics::RequestContext,
) -> Result<Option<String>> {
    exchange(
        cache,
        &DaemonRequest::Arguments {
            context: context.clone(),
            args: args.to_vec(),
        },
    )
}

/// Stops a listening daemon through its authenticated-by-filesystem socket.
pub fn stop(cache: &Path) -> Result<String> {
    Ok(exchange(cache, &DaemonRequest::Stop)?
        .unwrap_or_else(|| "{\"status\":\"not_running\"}".to_owned()))
}

/// Sandboxed clients get EPERM/EACCES from `connect`; treat as "no daemon".
fn unreachable(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::NotFound | ErrorKind::ConnectionRefused | ErrorKind::PermissionDenied
    )
}

fn exchange(cache: &Path, request: &DaemonRequest) -> Result<Option<String>> {
    request.validate()?;
    let mut stream = match UnixStream::connect(cache.join(SOCKET_NAME)) {
        Ok(stream) => stream,
        Err(error) if unreachable(error.kind()) => {
            return Ok(None);
        }
        Err(error) => return Err(error).context("connect to repository daemon"),
    };
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
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

struct ReconcileSchedule {
    interval: Duration,
    last_reconcile: Instant,
    first_event: Option<Instant>,
    latest_event: Option<Instant>,
}

impl ReconcileSchedule {
    fn new(now: Instant, interval: Duration) -> Self {
        Self {
            interval,
            last_reconcile: now,
            first_event: None,
            latest_event: None,
        }
    }

    fn due(&mut self, now: Instant, changed: bool) -> bool {
        if changed {
            self.first_event.get_or_insert(now);
            self.latest_event = Some(now);
        }
        now.duration_since(self.last_reconcile) >= self.interval
            || self
                .latest_event
                .is_some_and(|time| now.duration_since(time) >= DEBOUNCE)
            || self
                .first_event
                .is_some_and(|time| now.duration_since(time) >= MAX_DEBOUNCE)
    }

    fn completed(&mut self) {
        self.last_reconcile = Instant::now();
        self.first_event = None;
        self.latest_event = None;
    }
}

/// Whether a watched path may affect the index: outside pruned build/VCS trees.
fn indexed_path(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root).map_or(true, |relative| {
        !relative.components().any(|part| {
            UNINDEXED_DIRECTORIES
                .iter()
                .any(|name| part.as_os_str() == *name)
        })
    })
}

/// Watching (indexing) daemons yield CPU and disk to interactive work and builds.
fn lower_priority() {
    // SAFETY: plain syscalls on the calling process with constant arguments.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
        // IOPRIO_WHO_PROCESS = 1; IOPRIO_CLASS_IDLE = 3 in the top three bits.
        libc::syscall(libc::SYS_ioprio_set, 1, 0, 3 << 13);
    }
}

fn watch(root: &Path, cache: &Path, dirty: Arc<AtomicBool>) -> Option<notify::RecommendedWatcher> {
    let cache = cache.to_owned();
    let watched_root = root.to_owned();
    let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let changed = match event {
            Ok(event) => {
                if event.need_rescan() {
                    eprintln!("trufflepig: watcher lost events; scheduling reconciliation");
                }
                event.need_rescan()
                    || (!matches!(event.kind, EventKind::Access(_))
                        && (event.paths.is_empty()
                            || event.paths.iter().any(|path| {
                                !path.starts_with(&cache) && indexed_path(&watched_root, path)
                            })))
            }
            Err(error) => {
                eprintln!("trufflepig: watcher failed; scheduling reconciliation: {error}");
                true
            }
        };
        if changed {
            dirty.store(true, Ordering::Release);
        }
    });
    match watcher.and_then(|mut watcher| {
        watcher.watch(root, RecursiveMode::Recursive)?;
        Ok(watcher)
    }) {
        Ok(watcher) => Some(watcher),
        Err(error) => {
            eprintln!("trufflepig: watcher unavailable; using periodic reconciliation: {error}");
            None
        }
    }
}

/// Daemon work is serialized; idle ticks do not reconcile the index.
pub enum DaemonEvent {
    Request {
        context: crate::diagnostics::RequestContext,
        args: Vec<String>,
    },
    Reconcile,
    Idle,
}

/// Calls the handler at startup, reconciliation, requests, and each idle tick;
/// returns `Ok` on its own once the watched root is no longer a directory.
pub fn serve(
    root: &Path,
    cache: &Path,
    handler: impl FnMut(DaemonEvent) -> Result<String>,
) -> Result<()> {
    serve_inner(root, cache, true, handler)
}

/// Serves a coordinator without watching or indexing a repository root.
pub fn serve_coordinator(
    root: &Path,
    cache: &Path,
    handler: impl FnMut(DaemonEvent) -> Result<String>,
) -> Result<()> {
    serve_inner(root, cache, false, handler)
}

fn serve_inner(
    root: &Path,
    cache: &Path,
    watching: bool,
    mut handler: impl FnMut(DaemonEvent) -> Result<String>,
) -> Result<()> {
    let root = root.canonicalize().context("resolve watched repository")?;
    fs::create_dir_all(cache)?;
    let cache = cache.canonicalize()?;
    let socket = DaemonSocket::bind(&cache)?;
    let dirty = Arc::new(AtomicBool::new(false));
    let watcher = if watching {
        lower_priority();
        watch(&root, &cache, Arc::clone(&dirty))
    } else {
        None
    };
    handler(DaemonEvent::Reconcile).context("initial repository reconciliation")?;
    let interval = if watcher.is_some() {
        WATCHED_RECONCILE_INTERVAL
    } else {
        RECONCILE_INTERVAL
    };
    let mut schedule = ReconcileSchedule::new(Instant::now(), interval);
    loop {
        if schedule.due(Instant::now(), dirty.swap(false, Ordering::AcqRel)) {
            // Returning drops the socket and releases `daemon.lock` for the router sweep.
            if watching && !root.is_dir() {
                eprintln!(
                    "trufflepig: root {} is gone; daemon exiting",
                    root.display()
                );
                return Ok(());
            }
            if let Err(error) = handler(DaemonEvent::Reconcile) {
                eprintln!(
                    "trufflepig: reconciliation failed; periodic retry remains active: {error:#}"
                );
            }
            schedule.completed();
        }
        match socket.listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_nonblocking(false)?;
                stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                stream.set_write_timeout(Some(Duration::from_secs(5)))?;
                let request = protocol::read_request(&mut stream);
                let stopping = matches!(&request, Ok(DaemonRequest::Stop));
                let result = match request {
                    Ok(DaemonRequest::Arguments { context, args }) => {
                        handler(DaemonEvent::Request { context, args })
                    }
                    Ok(DaemonRequest::Stop) => Ok("{\"status\":\"stopped\"}".to_owned()),
                    Err(error) => Err(error),
                };
                let reply = match result {
                    Ok(output) => DaemonReply::Success { output },
                    Err(error) => DaemonReply::Failure {
                        message: format!("{error:#}"),
                    },
                };
                if let Err(error) = protocol::write_reply(&mut stream, &reply) {
                    eprintln!("trufflepig: daemon response failed: {error:#}");
                }
                if stopping {
                    return Ok(());
                }
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                let _ = handler(DaemonEvent::Idle);
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(error).context("accept daemon request"),
        }
    }
}
