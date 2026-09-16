//! File-spool transport for clients whose sandbox denies unix-socket connects.
//!
//! A client drops `<request_id>.request` into the spool directory and polls for
//! `<request_id>.reply`; the serving daemon drains requests on its idle tick and
//! answers by atomic rename. Both files carry the socket protocol's JSON bodies.
//! A `heartbeat` file, refreshed while the daemon serves, tells clients whether
//! anyone is draining the spool before they wait.

use super::protocol::{DaemonReply, DaemonRequest};
use anyhow::{Context, Result, bail};
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

const HEARTBEAT: &str = "heartbeat";
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const HEARTBEAT_STALE: Duration = Duration::from_secs(5);
const REPLY_WAIT: Duration = Duration::from_secs(120);
const REPLY_POLL: Duration = Duration::from_millis(20);
const ORPHAN_AGE: Duration = Duration::from_secs(300);

/// Sends a request through the spool; `None` means no daemon drains it.
pub fn request(
    spool: &Path,
    args: &[String],
    context: &crate::diagnostics::RequestContext,
) -> Result<Option<String>> {
    let request = DaemonRequest::Arguments {
        context: context.clone(),
        args: args.to_vec(),
    };
    request.validate()?;
    if !heartbeat_alive(spool) {
        return Ok(None);
    }
    let id = uuid::Uuid::parse_str(&context.request_id)?.to_string();
    let request_path = spool.join(format!("{id}.request"));
    let reply_path = spool.join(format!("{id}.reply"));
    if let Err(error) = write_atomic(&request_path, &serde_json::to_vec(&request)?) {
        if matches!(
            error.kind(),
            ErrorKind::NotFound | ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem
        ) {
            return Ok(None);
        }
        return Err(error).context("write spooled daemon request");
    }
    let deadline = Instant::now() + REPLY_WAIT;
    while Instant::now() < deadline {
        match fs::read(&reply_path) {
            Ok(bytes) => {
                let _ = fs::remove_file(&reply_path);
                let reply: DaemonReply =
                    serde_json::from_slice(&bytes).context("decode spooled daemon reply")?;
                return match reply {
                    DaemonReply::Success { output } => Ok(Some(output)),
                    DaemonReply::Failure { message } => bail!("daemon: {message}"),
                };
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("read spooled daemon reply"),
        }
        if !heartbeat_alive(spool) && fs::remove_file(&request_path).is_ok() {
            return Ok(None);
        }
        std::thread::sleep(REPLY_POLL);
    }
    let _ = fs::remove_file(&request_path);
    bail!("daemon_unavailable: spooled request timed out")
}

/// Serves one spool directory from a daemon's idle ticks.
pub struct SpoolServer {
    dir: PathBuf,
    heartbeat_at: Option<Instant>,
}

impl SpoolServer {
    /// Creates the private spool directory and discards leftovers.
    pub fn open(dir: &Path) -> Result<Self> {
        if let Some(parent) = dir.parent() {
            fs::create_dir_all(parent)?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        fs::create_dir_all(dir).context("create daemon spool directory")?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        for entry in fs::read_dir(dir)? {
            let _ = fs::remove_file(entry?.path());
        }
        let mut server = Self {
            dir: dir.to_owned(),
            heartbeat_at: None,
        };
        server.beat()?;
        Ok(server)
    }

    /// Refreshes the heartbeat and answers every pending request in order.
    pub fn drain(
        &mut self,
        mut handler: impl FnMut(crate::diagnostics::RequestContext, Vec<String>) -> Result<String>,
    ) {
        if let Err(error) = self.beat() {
            eprintln!("trufflepig: spool heartbeat failed: {error:#}");
        }
        let mut pending = Vec::new();
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match request_id(&path, "request") {
                Some(id) => pending.push((id, path)),
                None => remove_orphan(&path),
            }
        }
        pending.sort();
        for (id, path) in pending {
            let result = fs::read(&path)
                .context("read spooled daemon request")
                .and_then(|bytes| {
                    serde_json::from_slice::<DaemonRequest>(&bytes).context("decode spooled request")
                })
                .and_then(|request| {
                    request.validate()?;
                    match request {
                        DaemonRequest::Arguments { context, args } => handler(context, args),
                        DaemonRequest::Stop => bail!("spooled requests cannot stop a daemon"),
                    }
                });
            let _ = fs::remove_file(&path);
            let reply = match result {
                Ok(output) => DaemonReply::Success { output },
                Err(error) => DaemonReply::Failure {
                    message: format!("{error:#}"),
                },
            };
            let reply_path = self.dir.join(format!("{id}.reply"));
            let written = serde_json::to_vec(&reply)
                .map_err(anyhow::Error::from)
                .and_then(|bytes| write_atomic(&reply_path, &bytes).map_err(Into::into));
            if let Err(error) = written {
                eprintln!("trufflepig: spooled reply failed: {error:#}");
            }
        }
    }

    fn beat(&mut self) -> Result<()> {
        if self
            .heartbeat_at
            .is_some_and(|at| at.elapsed() < HEARTBEAT_INTERVAL)
        {
            return Ok(());
        }
        let path = self.dir.join(HEARTBEAT);
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)?;
        file.set_modified(SystemTime::now())?;
        self.heartbeat_at = Some(Instant::now());
        Ok(())
    }
}

/// Returns the request UUID when `path` is `<uuid>.<extension>`.
fn request_id(path: &Path, extension: &str) -> Option<String> {
    if path.extension()? != extension {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    uuid::Uuid::parse_str(stem).ok().map(|id| id.to_string())
}

/// Removes replies and stray files nobody collected within the orphan age.
fn remove_orphan(path: &Path) {
    if path.file_name().is_some_and(|name| name == HEARTBEAT) {
        return;
    }
    let old = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age > ORPHAN_AGE);
    if old {
        let _ = fs::remove_file(path);
    }
}

fn heartbeat_alive(spool: &Path) -> bool {
    fs::metadata(spool.join(HEARTBEAT))
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age < HEARTBEAT_STALE)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    fs::write(&temporary, bytes)?;
    fs::rename(&temporary, path)
}
