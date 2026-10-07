//! Bounded daemon replies and isolated environments for real CLI routing tests.

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const CHILD_CASE: &str = "TRUFFLEPIG_SPOOL_FALLBACK_CASE";
const FIXTURE_DIR: &str = "TRUFFLEPIG_SPOOL_FALLBACK_DIR";
pub(super) const DIRECT_REPLY: &str = "{\"status\":\"direct_spool_fallback\"}";

pub(super) fn in_child(case: &str) -> Result<bool> {
    if std::env::var(CHILD_CASE).as_deref() == Ok(case) {
        return Ok(true);
    }
    let directory = crate::board::board_test_support::scratch("sf-");
    for name in ["r", "c", "d", "s"] {
        fs::create_dir(directory.path().join(name))?;
    }
    fs::write(directory.path().join("s/heartbeat"), b"")?;
    let mut child = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            &format!("cli::tests::spool_fallback_tests::{case}"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_CASE, case)
        .env(FIXTURE_DIR, directory.path())
        .env("TRUFFLEPIG_SYSTEM_DIR", directory.path().join("d"))
        .env("TRUFFLEPIG_SPOOL_DIR", directory.path().join("s"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let timed_out = loop {
        if child.try_wait()?.is_some() {
            break false;
        }
        if Instant::now() >= deadline {
            child.kill()?;
            break true;
        }
        thread::sleep(Duration::from_millis(5));
    };
    let output = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("{stdout}{stderr}");
    ensure!(!timed_out, "CLI fixture exceeded five seconds");
    ensure!(
        output.status.success(),
        "CLI fixture failed: {stdout}{stderr}"
    );
    ensure!(
        stdout.contains("running 1 test") && stdout.contains("1 passed; 0 failed; 0 ignored"),
        "child must execute the requested test: {stdout}"
    );
    Ok(false)
}

pub(super) fn fixture_dir() -> PathBuf {
    PathBuf::from(std::env::var_os(FIXTURE_DIR).expect("isolated child fixture directory"))
}

pub(super) fn args() -> Vec<String> {
    let directory = fixture_dir();
    vec![
        "--no-workspace".into(),
        "--root".into(),
        directory.join("r").display().to_string(),
        "--cache".into(),
        directory.join("c").display().to_string(),
        "status".into(),
    ]
}

pub(super) struct StubDaemon {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<Option<Value>>>>,
}

impl StubDaemon {
    pub(super) fn root() -> Result<Self> {
        Self::start(
            &fixture_dir().join("c"),
            json!({"status": "success", "output": DIRECT_REPLY}),
        )
    }

    pub(super) fn router_timeout(message: &str) -> Result<Self> {
        Self::start(
            &fixture_dir().join("d"),
            json!({"status": "failure", "message": message}),
        )
    }

    fn start(directory: &Path, reply: Value) -> Result<Self> {
        let listener = UnixListener::bind(directory.join(crate::daemon::SOCKET_NAME))?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if stopping.load(Ordering::SeqCst) {
                    return Ok(None);
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(Duration::from_millis(250)))?;
                        stream.set_write_timeout(Some(Duration::from_millis(250)))?;
                        let mut length = [0; 4];
                        stream.read_exact(&mut length)?;
                        let length = u32::from_be_bytes(length) as usize;
                        ensure!(length <= 4096, "unexpected fixture request size: {length}");
                        let mut bytes = vec![0; length];
                        stream.read_exact(&mut bytes)?;
                        let request = serde_json::from_slice(&bytes)?;
                        let reply = serde_json::to_vec(&reply)?;
                        stream.write_all(&(reply.len() as u32).to_be_bytes())?;
                        stream.write_all(&reply)?;
                        return Ok(Some(request));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(error) => return Err(error.into()),
                }
                if Instant::now() >= deadline {
                    bail!("daemon fixture exceeded two seconds waiting for a request");
                }
                thread::sleep(Duration::from_millis(1));
            }
        });
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }

    pub(super) fn finish(mut self) -> Result<Option<Value>> {
        self.stop.store(true, Ordering::SeqCst);
        self.worker
            .take()
            .unwrap()
            .join()
            .map_err(|_| anyhow::anyhow!("fixture worker panicked"))?
            .context("daemon fixture")
    }
}

impl Drop for StubDaemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
