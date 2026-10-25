//! Root daemon leases renew independently of foreground request duration.

use anyhow::Result;
use std::path::Path;
use std::sync::mpsc::{Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

/// Keeps a root's worker lease alive until daemon shutdown.
pub struct Heartbeat {
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Heartbeat {
    pub fn start(root: &Path, cache: &Path) -> Result<Self> {
        super::start(root, cache)?;
        let root = root.canonicalize()?;
        let cache = cache.canonicalize()?;
        Self::spawn(super::POLL_INTERVAL, move || {
            if let Err(error) = super::start(&root, &cache) {
                eprintln!("trufflepig: history heartbeat failed: {error:#}");
            }
        })
    }

    fn spawn(interval: Duration, mut renew: impl FnMut() + Send + 'static) -> Result<Self> {
        let (stop, receive) = channel();
        let thread = std::thread::Builder::new()
            .name("history-heartbeat".into())
            .spawn(move || {
                while matches!(
                    receive.recv_timeout(interval),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                ) {
                    renew();
                }
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

/// Longest wait for a renewal in flight before the thread is detached.
const STOP_GRACE: Duration = Duration::from_secs(3);

impl Drop for Heartbeat {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        let Some(thread) = self.thread.take() else {
            return;
        };
        // A renewal blocked on a contended lease lock must not pin a daemon
        // whose root is gone; the thread dies with the process either way.
        let deadline = std::time::Instant::now() + STOP_GRACE;
        while !thread.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if thread.is_finished() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn heartbeat_runs_while_caller_is_busy_and_drop_stops_it() -> Result<()> {
        let calls = Arc::new(AtomicUsize::new(0));
        let background_calls = calls.clone();
        let (tick, receive) = channel();
        let heartbeat = Heartbeat::spawn(Duration::from_millis(5), move || {
            background_calls.fetch_add(1, Ordering::SeqCst);
            let _ = tick.send(());
        })?;
        receive.recv_timeout(Duration::from_secs(1))?;
        drop(heartbeat);
        let stopped = calls.load(Ordering::SeqCst);
        assert!(stopped > 0);
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(calls.load(Ordering::SeqCst), stopped);
        Ok(())
    }

    #[test]
    fn drop_detaches_a_renewal_that_never_returns() -> Result<()> {
        let (started, wait_started) = channel();
        let heartbeat = Heartbeat::spawn(Duration::from_millis(5), move || {
            let _ = started.send(());
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        })?;
        wait_started.recv_timeout(Duration::from_secs(1))?;
        let start = std::time::Instant::now();
        drop(heartbeat);
        let elapsed = start.elapsed();
        assert!(elapsed >= STOP_GRACE && elapsed < STOP_GRACE + Duration::from_secs(1));
        Ok(())
    }

    #[test]
    fn drop_wakes_a_long_heartbeat_wait() -> Result<()> {
        let heartbeat = Heartbeat::spawn(Duration::from_secs(60), || {})?;
        let start = std::time::Instant::now();
        drop(heartbeat);
        assert!(start.elapsed() < Duration::from_secs(1));
        Ok(())
    }
}
