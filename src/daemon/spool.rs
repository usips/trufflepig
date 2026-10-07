//! File-spool transport for clients whose sandbox denies unix-socket connects.
//!
//! A client drops `<request_id>.request` into the spool directory and polls for
//! `<request_id>.reply`; the serving daemon claims each request by renaming it to
//! `<request_id>.claimed`, answers it on a worker, and publishes the reply by
//! atomic rename. Files carry the socket protocol's JSON bodies. A `heartbeat`
//! file, refreshed while the daemon serves, tells clients whether anyone is
//! draining the spool before they wait.

use super::deadline::{self, QueryDeadline, TIMED_OUT};
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

/// Sends a request until `deadline`; `None` means no daemon drains it.
pub fn request(
    spool: &Path,
    args: &[String],
    context: &crate::diagnostics::RequestContext,
    deadline: QueryDeadline,
) -> Result<Option<String>> {
    check_deadline(&deadline)?;
    let request = DaemonRequest::Arguments {
        context: context.clone(),
        args: args.to_vec(),
    };
    let validation = request.validate();
    check_deadline(&deadline)?;
    validation?;
    let serialized = serde_json::to_vec(&request);
    check_deadline(&deadline)?;
    let bytes = serialized?;

    let parsed_id = uuid::Uuid::parse_str(&context.request_id);
    check_deadline(&deadline)?;
    let id = parsed_id?.to_string();
    let request_path = spool.join(format!("{id}.request"));
    let reply_path = spool.join(format!("{id}.reply"));
    let result = request_until(spool, &request_path, &reply_path, &bytes, deadline);
    if result.as_ref().err().is_some_and(deadline::is_timed_out) {
        remove_pending_request(&request_path);
    }
    result
}

fn request_until(
    spool: &Path,
    request_path: &Path,
    reply_path: &Path,
    bytes: &[u8],
    deadline: QueryDeadline,
) -> Result<Option<String>> {
    if !heartbeat_alive(spool, &deadline)? {
        return Ok(None);
    }
    check_deadline(&deadline)?;
    let write_result = write_request_atomic(request_path, bytes, &deadline, |temporary, bytes| {
        fs::write(temporary, bytes)
    });
    check_deadline(&deadline)?;
    if let Err(error) = write_result {
        let spool_unavailable = error.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<std::io::Error>(),
                Some(io)
                    if matches!(
                        io.kind(),
                        ErrorKind::NotFound
                            | ErrorKind::PermissionDenied
                            | ErrorKind::ReadOnlyFilesystem
                    )
            )
        });
        if spool_unavailable {
            return Ok(None);
        }
        return Err(error).context("write spooled daemon request");
    }
    loop {
        check_deadline(&deadline)?;
        let read_result = fs::read(reply_path);
        check_deadline(&deadline)?;
        match read_result {
            Ok(bytes) => {
                let decoded = serde_json::from_slice(&bytes).context("decode spooled daemon reply");
                check_deadline(&deadline)?;
                let reply: DaemonReply = decoded?;
                let _ = fs::remove_file(reply_path);
                check_deadline(&deadline)?;
                return match reply {
                    DaemonReply::Success { output } => Ok(Some(output)),
                    DaemonReply::Failure { message } => bail!("daemon: {message}"),
                };
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("read spooled daemon reply"),
        }
        if !heartbeat_alive(spool, &deadline)? {
            check_deadline(&deadline)?;
            if fs::remove_file(request_path).is_ok() {
                check_deadline(&deadline)?;
                return Ok(None);
            }
            // Claimed by a router that stopped beating: no reply is coming.
            if let Some(id) = request_id(request_path, "request") {
                let _ = fs::remove_file(spool.join(format!("{id}.claimed")));
            }
            check_deadline(&deadline)?;
            bail!("daemon_unavailable: router stopped while answering a spooled request");
        }
        let wait = deadline.cap(REPLY_POLL);
        if wait.is_zero() {
            check_deadline(&deadline)?;
        }
        std::thread::sleep(wait);
    }
}

fn check_deadline(deadline: &QueryDeadline) -> Result<()> {
    if deadline.expired() {
        bail!("{TIMED_OUT}: spooled request deadline expired");
    }
    Ok(())
}

fn remove_pending_request(path: &Path) {
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(temporary_path(path));
}

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
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn request_publication_does_not_rename_after_temp_write_expires_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let request_path = dir.path().join("request.request");
        let temporary = temporary_path(&request_path);
        let deadline = QueryDeadline::after(Duration::from_millis(250));
        let mut wrote_temporary = false;

        let error =
            write_request_atomic(&request_path, b"request", &deadline, |temporary, bytes| {
                fs::write(temporary, bytes)?;
                wrote_temporary = true;
                while !deadline.expired() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(())
            })
            .unwrap_err();

        assert!(wrote_temporary, "test did not reach the temporary write");
        assert!(deadline::is_timed_out(&error), "{error:#}");
        assert!(!request_path.exists(), "expired request was published");
        assert!(
            !temporary.exists(),
            "expired temporary request was retained"
        );
    }

    #[test]
    fn router_restart_and_orphan_sweep_preserve_durable_feedback() {
        let dir = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let durable = ["feedback", "feedback.quarantine"];
        let modified = SystemTime::now() - ORPHAN_AGE - Duration::from_secs(1);
        for extension in durable {
            let path = dir.path().join(format!("{id}.{extension}"));
            fs::write(&path, b"durable report").unwrap();
            File::open(&path).unwrap().set_modified(modified).unwrap();
        }
        let transient = dir.path().join(format!("{id}.reply"));
        fs::write(&transient, b"reply").unwrap();
        let mut server = SpoolServer::open(dir.path()).unwrap();
        assert!(!transient.exists());
        assert!(server.claim().is_empty());
        for extension in durable {
            assert!(dir.path().join(format!("{id}.{extension}")).exists());
        }
    }

    #[test]
    fn restart_reaps_abandoned_pending_and_expires_quarantine_after_thirty_days() {
        let dir = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let cases = [
            (
                format!("{id}.feedback"),
                QUARANTINE_AGE + Duration::from_secs(1),
                true,
            ),
            (
                format!("{id}.feedback.pending"),
                ORPHAN_AGE + Duration::from_secs(1),
                false,
            ),
            (
                format!("{id}.feedback.quarantine"),
                ORPHAN_AGE + Duration::from_secs(1),
                true,
            ),
            (
                format!("{id}.feedback.old.quarantine"),
                QUARANTINE_AGE + Duration::from_secs(1),
                false,
            ),
            (
                "unknown.old".to_owned(),
                ORPHAN_AGE + Duration::from_secs(1),
                false,
            ),
            ("unknown.fresh".to_owned(), Duration::ZERO, true),
            (format!("{id}.feedback.fresh.pending"), Duration::ZERO, true),
        ];
        for (name, age, _) in &cases {
            let path = dir.path().join(name);
            fs::write(&path, b"fixture").unwrap();
            File::open(&path)
                .unwrap()
                .set_modified(SystemTime::now() - *age)
                .unwrap();
        }
        let mut server = SpoolServer::open(dir.path()).unwrap();
        assert!(server.claim().is_empty());
        for (name, _, retained) in cases {
            assert_eq!(dir.path().join(&name).exists(), retained, "{name}");
        }
    }

    #[test]
    fn orphan_sweep_removes_only_expired_transport_files() {
        let dir = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let old = dir.path().join(format!("{id}.reply.tmp"));
        let fresh = dir.path().join(format!("{id}.reply"));
        fs::write(&old, b"old reply").unwrap();
        fs::write(&fresh, b"fresh reply").unwrap();
        File::open(&old)
            .unwrap()
            .set_modified(SystemTime::now() - ORPHAN_AGE - Duration::from_secs(1))
            .unwrap();
        remove_orphan(&old);
        remove_orphan(&fresh);
        assert!(!old.exists());
        assert!(fresh.exists());
    }
}
