use crate::coded_error::CodedContext;
use anyhow::Result;
use std::os::unix::process::CommandExt;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    error::Error,
    fmt, io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
};

#[cfg(test)]
mod tests;

const DELETED_EXECUTABLE_SUFFIX: &[u8] = b" (deleted)";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BackgroundOwner {
    Command,
    SystemRouter,
    RootDaemon,
    WorkspaceCoordinator,
    InferenceClient,
}

impl BackgroundOwner {
    fn role(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::SystemRouter => "system router",
            Self::RootDaemon => "root daemon",
            Self::WorkspaceCoordinator => "workspace coordinator",
            Self::InferenceClient => "inference client",
        }
    }

    fn recovery(self) -> &'static str {
        match self {
            Self::Command => "rerun the command with the current trufflepig binary",
            Self::SystemRouter => {
                "restart the owning unit with `systemctl --user restart trufflepig-system.service`; if detached, stop the detached router with `trufflepig system stop`, then run `trufflepig system ensure` (which starts the managed unit when it is installed)"
            }
            Self::RootDaemon => {
                "restart the owning systemd unit, or stop the detached root daemon for this cache and retry with the current trufflepig binary"
            }
            Self::WorkspaceCoordinator => {
                "restart the owning systemd unit, or stop the detached workspace coordinator for this cache and retry with the current trufflepig binary"
            }
            Self::InferenceClient => {
                "restart the process through its owning systemd unit, or rerun the caller with the current trufflepig binary"
            }
        }
    }
}

/// A background owner cannot spawn children from a replaced executable inode.
#[derive(Debug)]
pub(crate) struct ReplacedExecutable {
    pub(crate) executable: PathBuf,
    pub(crate) cache: PathBuf,
    pub(crate) owner: BackgroundOwner,
}

impl fmt::Display for ReplacedExecutable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "router_binary_replaced: executable {} for cache {} was deleted after the {} started; {}",
            self.executable.display(),
            self.cache.display(),
            self.owner.role(),
            self.owner.recovery(),
        )
    }
}

impl Error for ReplacedExecutable {}

/// Returns the process executable for child spawns, with owner-specific advice.
pub(crate) fn current_background_executable(
    cache: &Path,
    owner: BackgroundOwner,
) -> Result<PathBuf> {
    let executable = std::env::current_exe().with_coded_context(|| {
        coded!(
            BackgroundExecutableUnavailable,
            "cannot determine executable owned by {} for cache {}",
            owner.role(),
            cache.display()
        )
    })?;
    validate_background_executable(executable, cache, owner)
}

/// Returns whether the raw Linux executable path carries the kernel suffix.
pub(crate) fn background_executable_replaced(executable: &Path) -> bool {
    executable
        .as_os_str()
        .as_bytes()
        .ends_with(DELETED_EXECUTABLE_SUFFIX)
}

fn validate_background_executable(
    executable: PathBuf,
    cache: &Path,
    owner: BackgroundOwner,
) -> Result<PathBuf> {
    if background_executable_replaced(&executable) {
        return Err(ReplacedExecutable {
            executable,
            cache: cache.to_owned(),
            owner,
        }
        .into());
    }
    Ok(executable)
}

/// Background spawns this process performed; tests assert a waiting client
/// spawns no second daemon.
#[cfg(test)]
static SPAWNED: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
pub(crate) fn spawned_count() -> u64 {
    SPAWNED.load(Ordering::Relaxed)
}

/// A detached daemon whose exit is reaped by its own waiter thread, so no
/// process-wide `waitpid(-1)` can steal another thread's child status.
pub(crate) struct BackgroundChild {
    #[cfg(test)]
    pid: u32,
    exit: mpsc::Receiver<()>,
}

impl BackgroundChild {
    #[cfg(test)]
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// Whether the process has exited and been reaped.
    pub fn exited(&self) -> bool {
        !matches!(self.exit.try_recv(), Err(mpsc::TryRecvError::Empty))
    }
}

/// Spawn a daemon in its own process group with no inherited standard streams.
pub(crate) fn spawn_background(command: &mut Command) -> io::Result<BackgroundChild> {
    let mut child = command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    #[cfg(test)]
    let pid = child.id();
    #[cfg(test)]
    SPAWNED.fetch_add(1, Ordering::Relaxed);
    let (exited, exit) = mpsc::channel();
    std::thread::Builder::new()
        .name("trufflepig-reaper".into())
        .stack_size(64 * 1024)
        .spawn(move || {
            let _ = child.wait();
            let _ = exited.send(());
        })?;
    Ok(BackgroundChild {
        #[cfg(test)]
        pid,
        exit,
    })
}

/// Spawns a background process with role and cache details in failures.
pub(crate) fn spawn_background_for(
    command: &mut Command,
    executable: &Path,
    role: &str,
    cache: &Path,
) -> Result<BackgroundChild> {
    spawn_background(command).with_coded_context(|| {
        coded!(
            BackgroundSpawnFailed,
            "cannot start {role} executable {} for cache {}",
            executable.display(),
            cache.display()
        )
    })
}
