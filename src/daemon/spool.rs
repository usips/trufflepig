//! File-spool transport for clients whose sandbox denies unix-socket connects.
//!
//! A client drops `<request_id>.request` into the spool directory and polls for
//! `<request_id>.reply`; the serving daemon claims each request by renaming it to
//! `<request_id>.claimed`, answers it on a worker, and publishes the reply by
//! atomic rename. Files carry the socket protocol's JSON bodies. A `heartbeat`
//! file, refreshed while the daemon serves, tells clients whether anyone is
//! draining the spool before they wait.

use super::deadline::{self, QueryDeadline};
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
const REPLY_POLL: Duration = Duration::from_millis(20);
const ORPHAN_AGE: Duration = Duration::from_secs(300);
const QUARANTINE_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

mod spool_client;
#[cfg(test)]
mod spool_test_deadline;
mod spool_timeout;

use spool_client::check_deadline;
pub use spool_client::request;
#[cfg(test)]
pub(crate) use spool_test_deadline::{expire_after_next_publication, publication_expired};
pub(crate) use spool_timeout::is_local_spool_timeout;

/// Serves one spool directory from a daemon's maintenance ticks.
pub struct SpoolServer {
    dir: PathBuf,
    heartbeat_at: Option<Instant>,
}

impl SpoolServer {
    /// Creates the private spool directory and discards transient leftovers.
    pub fn open(dir: &Path) -> Result<Self> {
        if let Some(parent) = dir.parent() {
            fs::create_dir_all(parent)?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        fs::create_dir_all(dir).context("create daemon spool directory")?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if transient_file(&path) {
                let _ = fs::remove_file(path);
            } else {
                remove_orphan(&path);
            }
        }
        let mut server = Self {
            dir: dir.to_owned(),
            heartbeat_at: None,
        };
        server.beat()?;
        Ok(server)
    }

    /// Refreshes the heartbeat and claims every pending request in id order; a
    /// claimed request is never returned again, even before it is answered.
    pub fn claim(&mut self) -> Vec<ClaimedSpoolRequest> {
        if let Err(error) = self.beat() {
            eprintln!("trufflepig: spool heartbeat failed: {error:#}");
        }
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut pending = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            match request_id(&path, "request") {
                Some(id) => pending.push((id, path)),
                None => remove_orphan(&path),
            }
        }
        pending.sort();
        let mut claimed = Vec::with_capacity(pending.len());
        for (id, path) in pending {
            let claim = self.dir.join(format!("{id}.claimed"));
            // A request the client withdrew (timeout, dead heartbeat) is gone.
            if fs::rename(&path, &claim).is_ok() {
                claimed.push(ClaimedSpoolRequest {
                    reply: self.dir.join(format!("{id}.reply")),
                    claim,
                });
            }
        }
        claimed
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

/// A request this server claimed; answering or refusing it publishes its reply.
pub struct ClaimedSpoolRequest {
    claim: PathBuf,
    reply: PathBuf,
}

impl ClaimedSpoolRequest {
    /// Decodes and validates the request, runs `handler`, and publishes its reply.
    pub fn answer(
        self,
        handler: impl FnOnce(crate::diagnostics::RequestContext, Vec<String>) -> Result<String>,
    ) {
        let result = fs::read(&self.claim)
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
        self.publish(DaemonReply::from_result(result));
    }

    /// Publishes a failure without running the request.
    pub fn refuse(self, message: &str) {
        self.publish(DaemonReply::Failure {
            message: message.to_owned(),
        });
    }

    fn publish(self, reply: DaemonReply) {
        let _ = fs::remove_file(&self.claim);
        let written = serde_json::to_vec(&reply)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| write_atomic(&self.reply, &bytes).map_err(Into::into));
        if let Err(error) = written {
            eprintln!("trufflepig: spooled reply failed: {error:#}");
        }
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

/// Keeps published feedback until import and quarantines for thirty days.
fn remove_orphan(path: &Path) {
    if path.file_name().is_some_and(|name| name == HEARTBEAT)
        || path
            .extension()
            .is_some_and(|extension| extension == "feedback")
    {
        return;
    }
    let maximum_age = if path
        .extension()
        .is_some_and(|extension| extension == "quarantine")
    {
        QUARANTINE_AGE
    } else {
        ORPHAN_AGE
    };
    let old = fs::symlink_metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age > maximum_age);
    if old {
        let _ = fs::remove_file(path);
    }
}

fn transient_file(path: &Path) -> bool {
    if ["request", "reply", "claimed"]
        .into_iter()
        .any(|extension| request_id(path, extension).is_some())
    {
        return true;
    }
    if path.extension().is_some_and(|extension| extension == "tmp") {
        return path.file_stem().is_some_and(|stem| {
            let original = Path::new(stem);
            ["request", "reply", "claimed"]
                .into_iter()
                .any(|extension| request_id(original, extension).is_some())
        });
    }
    false
}

fn heartbeat_alive(spool: &Path, deadline: &QueryDeadline) -> Result<bool> {
    check_deadline(deadline)?;
    let metadata = fs::metadata(spool.join(HEARTBEAT));
    check_deadline(deadline)?;
    Ok(metadata
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age < HEARTBEAT_STALE))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    fs::write(&temporary, bytes)?;
    fs::rename(&temporary, path)
}

fn write_request_atomic(
    path: &Path,
    bytes: &[u8],
    deadline: &QueryDeadline,
    write_temporary: impl FnOnce(&Path, &[u8]) -> std::io::Result<()>,
) -> Result<()> {
    check_deadline(deadline)?;
    let temporary = temporary_path(path);
    check_deadline(deadline)?;
    let write_result = write_temporary(&temporary, bytes);
    if let Err(error) = write_result {
        if let Err(timeout) = check_deadline(deadline) {
            let _ = fs::remove_file(&temporary);
            return Err(timeout);
        }
        let _ = fs::remove_file(&temporary);
        return Err(error).context("write spooled daemon request");
    }
    if let Err(timeout) = check_deadline(deadline) {
        let _ = fs::remove_file(&temporary);
        return Err(timeout);
    }
    fs::rename(&temporary, path).context("publish spooled daemon request")?;
    Ok(())
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    PathBuf::from(temporary)
}

#[cfg(test)]
mod tests;
