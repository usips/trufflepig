//! Exercise the installed process boundary, including startup migration readiness.
#![cfg(target_os = "linux")]

use std::{
    os::{fd::AsRawFd, unix::net::UnixStream},
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};
use trufflepig::{
    board::{BOARD_API, SCHEMA_VERSION},
    daemon::{self, deadline::QueryDeadline},
    diagnostics::RequestContext,
};

struct EnsureProcess(Option<Child>);

impl EnsureProcess {
    fn child(&mut self) -> &mut Child {
        self.0.as_mut().unwrap()
    }

    fn finish(mut self, deadline: Instant) -> Output {
        while self.child().try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "system ensure did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
        self.0.take().unwrap().wait_with_output().unwrap()
    }
}

impl Drop for EnsureProcess {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct RouterProcess(Option<u32>);

impl RouterProcess {
    fn reap(&mut self, deadline: Instant) {
        let pid = self.0.unwrap();
        loop {
            let mut status = 0;
            let waited = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
            if waited == pid as libc::pid_t {
                self.0 = None;
                assert!(
                    libc::WIFEXITED(status),
                    "router did not exit normally: {status}"
                );
                assert_eq!(libc::WEXITSTATUS(status), 0);
                return;
            }
            if waited == -1 {
                let error = std::io::Error::last_os_error();
                assert_eq!(
                    error.kind(),
                    std::io::ErrorKind::Interrupted,
                    "reap router {pid}: {error}"
                );
            }
            assert!(
                Instant::now() < deadline,
                "spawned router {pid} did not stop"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for RouterProcess {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            let mut status = 0;
            let waited = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
            if waited == 0 {
                // The subreaper owns this child after the ensure process exits.
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
                loop {
                    let waited = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) };
                    if waited != -1
                        || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
                    {
                        break;
                    }
                }
            }
        }
    }
}

struct RouterSubreaper(libc::c_int);

impl RouterSubreaper {
    fn install() -> Self {
        let mut previous = 0;
        assert_eq!(
            unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut previous) },
            0
        );
        assert_eq!(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) }, 0);
        Self(previous)
    }
}

impl Drop for RouterSubreaper {
    fn drop(&mut self) {
        assert_eq!(
            unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, self.0) },
            0
        );
    }
}

fn peer_pid(stream: &UnixStream) -> u32 {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    assert_eq!(result, 0, "read router peer credentials");
    credentials.pid.try_into().unwrap()
}

fn scratch() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let parent = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME or TMPDIR required"))
                .join(".cache/codex-tmp")
        });
    assert!(parent.is_absolute() && !parent.starts_with("/tmp"));
    std::fs::create_dir_all(&parent).unwrap();
    tempfile::Builder::new()
        .prefix("router-process-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(parent)
        .unwrap()
}

#[test]
fn ensure_spawns_and_waits_for_a_real_router() {
    use std::os::unix::fs::PermissionsExt;
    let _subreaper = RouterSubreaper::install();
    let directory = scratch();
    let runtime = directory.path().join("runtime");
    let spool = directory.path().join("spool");
    let database = directory.path().join("board.sqlite3");
    let lock = rusqlite::Connection::open(&database).unwrap();
    std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600)).unwrap();
    lock.execute_batch("PRAGMA journal_mode=WAL; BEGIN IMMEDIATE")
        .unwrap();
    assert_eq!(
        lock.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0,
        "the router initializes an unmigrated database"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut router = RouterProcess(None);
    let mut ensuring = EnsureProcess(Some(
        Command::new(env!("CARGO_BIN_EXE_trufflepig"))
            .env("TRUFFLEPIG_SYSTEM_DIR", &runtime)
            .env("TRUFFLEPIG_SPOOL_DIR", &spool)
            .env("TRUFFLEPIG_BOARD_DB", &database)
            .env("XDG_CONFIG_HOME", directory.path().join("config"))
            .env("XDG_DATA_HOME", directory.path().join("data"))
            .env("XDG_CACHE_HOME", directory.path().join("cache"))
            .args(["system", "ensure"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    let stream = loop {
        if router.0.is_none() {
            let ensure_pid = ensuring.child().id();
            router.0 =
                std::fs::read_to_string(format!("/proc/{ensure_pid}/task/{ensure_pid}/children"))
                    .ok()
                    .and_then(|children| children.split_whitespace().next()?.parse().ok());
        }
        if let Ok(stream) = UnixStream::connect(runtime.join("daemon.sock")) {
            break stream;
        }
        assert!(Instant::now() < deadline, "spawned router did not bind");
        assert!(
            ensuring.child().try_wait().unwrap().is_none(),
            "ensure exited before bind"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let pid = peer_pid(&stream);
    if let Some(tracked) = router.0 {
        assert_eq!(tracked, pid, "socket peer is the tracked ensure child");
    }
    router.0 = Some(pid);
    println!(
        "ensure pid={} spawned router pid={pid}",
        ensuring.child().id()
    );
    assert_ne!(pid, std::process::id(), "router is a separate process");
    assert_ne!(
        pid,
        ensuring.child().id(),
        "ensure spawned the router child"
    );
    let error = daemon::request_by(
        &runtime,
        &["system".into(), "status".into()],
        &RequestContext::new(None, None),
        QueryDeadline::after(Duration::from_millis(100)),
    )
    .expect_err("the spawned router cannot answer status while migration is blocked");
    assert!(
        error.chain().any(|cause| {
            cause.downcast_ref::<std::io::Error>().is_some_and(|io| {
                matches!(
                    io.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                )
            })
        }),
        "{error:#}"
    );
    assert!(
        ensuring.child().try_wait().unwrap().is_none(),
        "ensure waits while migration is blocked"
    );
    drop(stream);
    lock.execute_batch("COMMIT").unwrap();
    let output = ensuring.finish(deadline);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reply = daemon::request_by(
        &runtime,
        &["system".into(), "status".into()],
        &RequestContext::new(None, None),
        QueryDeadline::after(Duration::from_secs(2)),
    )
    .unwrap()
    .expect("spawned router answers after ensure succeeds");
    let status: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(status["status"], "ok");
    assert_eq!(status["board_api"], BOARD_API);
    assert_eq!(status["schema_file"], SCHEMA_VERSION);
    assert_eq!(status["board_db"], serde_json::json!(database));
    daemon::stop(&runtime).unwrap();
    router.reap(Instant::now() + daemon::PROXY_REPLY_WAIT + Duration::from_secs(2));
    assert!(!runtime.join("daemon.sock").exists());
}
