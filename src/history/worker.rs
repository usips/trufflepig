//! One history worker per cache/common-directory pair, leased by root daemons.

mod heartbeat;
#[cfg(test)]
mod tests;
pub use heartbeat::Heartbeat;

use super::{History, git::GitRepository};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use notify::{EventKind, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt, process::CommandExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, sync_channel};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const REGISTRATION_TTL: Duration = Duration::from_secs(15);
const MAX_REGISTRATIONS: usize = 256;
const MAX_REGISTRATION_BYTES: u64 = 16 * 1024;
const REGISTRATIONS: &str = "registrations";

#[derive(Serialize, Deserialize)]
struct Registration {
    root: PathBuf,
    heartbeat_ms: u64,
}

struct HistoryLock(File);

impl std::ops::Deref for HistoryLock {
    type Target = File;
    fn deref(&self) -> &File {
        &self.0
    }
}

impl Drop for HistoryLock {
    fn drop(&mut self) {
        // Forked subprocesses can retain the open-file description before exec.
        // Explicit unlock releases ownership without waiting for those copies.
        let _ = FileExt::unlock(&self.0);
    }
}

/// Selects an isolated history directory, keyed by canonical Git common directory.
pub fn resolve_cache(
    root: &Path,
    live_override: Option<&Path>,
    history_override: Option<&Path>,
) -> Result<PathBuf> {
    let repository = GitRepository::discover(root)?;
    let base = if let Some(path) = history_override {
        path.to_owned()
    } else if let Some(path) = live_override {
        path.join("history")
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
            .context("history_cache_unavailable: set --history-cache or XDG_CACHE_HOME")?
            .join("trufflepig")
            .join("history")
    };
    Ok(base.join(
        blake3::hash(repository.common_dir.as_os_str().as_bytes())
            .to_hex()
            .as_str(),
    ))
}

fn private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn clock_ms() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64)
}

/// Renews a root daemon's fifteen-second lease without waiting for history work.
pub fn register(root: &Path, cache: &Path) -> Result<()> {
    let root = root
        .canonicalize()
        .context("resolve history registration root")?;
    private_directory(cache)?;
    let directory = cache.join(REGISTRATIONS);
    private_directory(&directory)?;
    active_roots(cache, clock_ms()?)?;
    let _guard = registration_lock(cache)?;
    let name = blake3::hash(root.as_os_str().as_bytes())
        .to_hex()
        .to_string();
    let path = directory.join(name);
    if !path.exists()
        && fs::read_dir(&directory)?.take(MAX_REGISTRATIONS).count() >= MAX_REGISTRATIONS
    {
        bail!("history_registration_limit: too many registered roots");
    }
    let record = serde_json::to_vec(&Registration {
        root,
        heartbeat_ms: clock_ms()?,
    })?;
    if record.len() as u64 > MAX_REGISTRATION_BYTES {
        bail!("history_registration_limit: root path exceeds registration bound");
    }
    let mut pending = tempfile::NamedTempFile::new_in(cache)?;
    pending.write_all(&record)?;
    pending.persist(path).context("publish history heartbeat")?;
    Ok(())
}

fn worker_lock(cache: &Path) -> Result<HistoryLock> {
    private_directory(cache)?;
    Ok(HistoryLock(
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(cache.join("worker.lock"))?,
    ))
}

fn registration_lock(cache: &Path) -> Result<HistoryLock> {
    let lock = HistoryLock(
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(cache.join("registrations.lock"))?,
    );
    lock.lock_exclusive()?;
    Ok(lock)
}

/// Reports advisory worker ownership without scheduling or creating a worker.
pub fn is_running(cache: &Path) -> Result<bool> {
    let lock = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(cache.join("worker.lock"))
    {
        Ok(lock) => HistoryLock(lock),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("inspect history worker lock"),
    };
    match lock.try_lock_exclusive() {
        Ok(()) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
        Err(error) => Err(error).context("inspect history worker ownership"),
    }
}

/// Registers a daemon and starts a detached worker when no worker owns the pair.
pub fn start(root: &Path, cache: &Path) -> Result<()> {
    register(root, cache)?;
    let lock = worker_lock(cache)?;
    match lock.try_lock_exclusive() {
        Ok(()) => drop(lock),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
        Err(error) => return Err(error).context("inspect history worker lock"),
    }
    let mut child = Command::new(std::env::current_exe()?)
        .arg("--root")
        .arg(root)
        .arg("--history-cache")
        .arg(cache.canonicalize()?)
        .arg("history-serve")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("start history worker")?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn active_roots(cache: &Path, now_ms: u64) -> Result<Vec<PathBuf>> {
    let _guard = registration_lock(cache)?;
    let directory = cache.join(REGISTRATIONS);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut roots = Vec::with_capacity(8);
    for entry in entries.take(MAX_REGISTRATIONS) {
        let path = entry?.path();
        let record = (|| -> Result<Registration> {
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.is_file() || metadata.len() > MAX_REGISTRATION_BYTES {
                bail!("invalid registration");
            }
            Ok(serde_json::from_slice(&fs::read(&path)?)?)
        })();
        match record {
            Ok(record)
                if now_ms.abs_diff(record.heartbeat_ms) <= REGISTRATION_TTL.as_millis() as u64 =>
            {
                roots.push(record.root);
            }
            _ => {
                let _ = fs::remove_file(path);
            }
        }
    }
    roots.sort_unstable();
    roots.dedup();
    Ok(roots)
}

fn ref_hints(common_dir: &Path) -> Option<(notify::RecommendedWatcher, Receiver<()>)> {
    let (send, receive) = sync_channel(1);
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.is_ok_and(|event| !matches!(event.kind, EventKind::Access(_))) {
            let _ = send.try_send(());
        }
    })
    .ok()?;
    watcher
        .watch(common_dir, RecursiveMode::NonRecursive)
        .ok()?;
    for child in ["refs", "worktrees"] {
        let path = common_dir.join(child);
        if path.exists() {
            let _ = watcher.watch(&path, RecursiveMode::Recursive);
        }
    }
    Some((watcher, receive))
}

/// Polls registered worktree tips; SQLite checkpoints survive worker termination.
pub fn serve(cache: &Path) -> Result<()> {
    let lock = worker_lock(cache)?;
    match lock.try_lock_exclusive() {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
        Err(error) => return Err(error).context("acquire history worker lock"),
    }
    let mut hints = None;
    loop {
        let roots = active_roots(cache, clock_ms()?)?;
        if roots.is_empty() {
            return Ok(());
        }
        for root in roots {
            let result = History::open(&root, cache).and_then(|mut history| {
                if hints.is_none() {
                    hints = ref_hints(&history.repository.common_dir);
                }
                history.index()
            });
            if let Err(error) = result {
                eprintln!("trufflepig: history indexing failed: {error:#}");
            }
        }
        if let Some((_, receiver)) = &hints {
            let _ = receiver.recv_timeout(POLL_INTERVAL);
            std::thread::sleep(Duration::from_millis(50));
        } else {
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}
