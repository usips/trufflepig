use std::os::unix::process::CommandExt;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    io,
    process::{Command, Stdio},
    sync::mpsc,
};

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

#[cfg(test)]
mod tests {
    use super::spawn_background;
    use std::process::Command;

    #[test]
    fn background_child_owns_process_group_and_is_reaped() {
        let mut command = Command::new("sleep");
        command.arg("60");
        let child = spawn_background(&mut command).expect("spawn background child");
        let child_pid = child.id() as libc::pid_t;
        let child_group = unsafe { libc::getpgid(child_pid) };
        let caller_group = unsafe { libc::getpgrp() };
        assert_eq!(child_group, child_pid);
        assert_ne!(child_group, caller_group);
        assert!(!child.exited());
        assert_eq!(unsafe { libc::kill(child_pid, libc::SIGKILL) }, 0);
        let started = std::time::Instant::now();
        while !child.exited() {
            assert!(started.elapsed() < std::time::Duration::from_secs(5));
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
