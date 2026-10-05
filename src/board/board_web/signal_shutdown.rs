//! Termination-signal cleanup for the foreground board web service.
//! SIGTERM/SIGINT/SIGHUP/SIGQUIT stay blocked in every thread; a dedicated
//! waiter consumes them with sigwait and runs the shutdown action, so systemd
//! stops, Ctrl-C, hangups, and quits all remove the published endpoint
//! before the process exits.

use anyhow::{Context, Result};
use std::thread::{self, JoinHandle};

const TERMINATION: [i32; 4] = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGQUIT];

/// Block termination signals in the calling thread; threads spawned later
/// inherit the mask, keeping the signals pending for the waiter instead of
/// killing the process outright. Call before spawning any thread.
pub(crate) fn block_termination() -> Result<()> {
    block(&TERMINATION)
}

/// Wait for a blocked termination signal, run `cleanup`, then exit with
/// success so a systemd stop is not treated as a failure.
pub(crate) fn spawn_exit_waiter(cleanup: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>> {
    spawn_waiter(&TERMINATION, move || {
        cleanup();
        std::process::exit(0);
    })
}

fn block(signals: &[i32]) -> Result<()> {
    let set = signal_set(signals)?;
    // SAFETY: set is fully initialized; pthread_sigmask changes only the
    // calling thread's signal mask.
    let result = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) };
    if result != 0 {
        return Err(std::io::Error::from_raw_os_error(result))
            .context("board_web_shutdown: block termination signals");
    }
    Ok(())
}

fn spawn_waiter(
    signals: &[i32],
    cleanup: impl FnOnce() + Send + 'static,
) -> Result<JoinHandle<()>> {
    let set = signal_set(signals)?;
    Ok(thread::spawn(move || {
        if wait_for_signal(&set) {
            cleanup();
        }
    }))
}

fn wait_for_signal(set: &libc::sigset_t) -> bool {
    let mut received = 0;
    // SAFETY: set is fully initialized and its signals are blocked in this
    // thread, which sigwait requires.
    unsafe { libc::sigwait(set, &mut received) == 0 }
}

fn signal_set(signals: &[i32]) -> Result<libc::sigset_t> {
    // SAFETY: set is zero-initialized, then filled by sigemptyset/sigaddset;
    // every signal number is a libc constant.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        if libc::sigemptyset(&mut set) != 0 {
            return Err(std::io::Error::last_os_error())
                .context("board_web_shutdown: build signal set");
        }
        for &signal in signals {
            if libc::sigaddset(&mut set, signal) != 0 {
                return Err(std::io::Error::last_os_error())
                    .context("board_web_shutdown: build signal set");
            }
        }
        Ok(set)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn blocked_termination_signal_reaches_the_waiter_and_runs_cleanup() {
        let directory = crate::board::board_test_support::scratch("web-signal-");
        let descriptor = directory.path().join("board-web.json");
        std::fs::write(&descriptor, "{}").unwrap();
        block(&[libc::SIGUSR1]).unwrap();
        let (sender, receiver) = mpsc::channel();
        let cleanup_path = descriptor.clone();
        let waiter = thread::spawn(move || {
            // SAFETY: pthread_self has no preconditions and cannot fail.
            sender.send(unsafe { libc::pthread_self() }).unwrap();
            let set = signal_set(&[libc::SIGUSR1]).unwrap();
            assert!(wait_for_signal(&set));
            std::fs::remove_file(&cleanup_path).unwrap();
        });
        let target = receiver.recv().unwrap();
        // SAFETY: target is the live pthread handle of our own waiter thread;
        // the signal is thread-directed, so no other thread can receive it.
        assert_eq!(unsafe { libc::pthread_kill(target, libc::SIGUSR1) }, 0);
        waiter.join().unwrap();
        assert!(!descriptor.exists(), "cleanup removed the descriptor");
        let set = signal_set(&[libc::SIGUSR1]).unwrap();
        // SAFETY: set is fully initialized; restoring the mask after the
        // consumed signal cannot deliver anything.
        assert_eq!(
            unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut()) },
            0
        );
    }
}
