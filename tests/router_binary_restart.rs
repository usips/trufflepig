//! A private router running from a scratch copy of the binary leaves for a
//! replacement only when systemd supervises it; clients run refused reads locally.
#![cfg(target_os = "linux")]

use std::os::unix::net::UnixListener;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};
use tp_core::daemon_transport::daemon_wire_protocol::{self, DaemonReply};
use tp_core::daemon_transport::{request_by, running, stop};
use tp_core::{query_deadline::QueryDeadline, request_context::RequestContext};

const RESTART_EXIT_STATUS: i32 = 75;

fn scratch() -> tempfile::TempDir {
    let parent = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME or TMPDIR required"))
                .join(".cache/codex-tmp")
        });
    assert!(parent.is_absolute() && !parent.starts_with("/tmp"));
    fs::create_dir_all(&parent).unwrap();
    tempfile::Builder::new()
        .prefix("router-restart-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(parent)
        .unwrap()
}

/// A router process plus the root daemon cache it may have spawned.
struct PrivateRouter {
    child: Child,
    runtime: PathBuf,
    daemon_cache: PathBuf,
}

impl PrivateRouter {
    fn start(directory: &Path, binary: &Path, supervised: bool) -> Self {
        let runtime = directory.join("runtime");
        let mut command = Command::new(binary);
        command
            .arg("system-serve")
            .env("TRUFFLEPIG_SYSTEM_DIR", &runtime)
            .env("TRUFFLEPIG_SPOOL_DIR", directory.join("spool"))
            .env("TRUFFLEPIG_BOARD_DB", directory.join("board.sqlite3"))
            .env("XDG_CONFIG_HOME", directory.join("config"))
            .env("XDG_DATA_HOME", directory.join("data"))
            .env("XDG_CACHE_HOME", directory.join("cache"))
            .env("TRUFFLEPIG_BINARY_SETTLE_MS", "1500")
            .env_remove("TRUFFLEPIG_DAEMON_SLICE")
            .env_remove("TRUFFLEPIG_DAEMON_SCOPE_FROM")
            .env_remove("TRUFFLEPIG_ROUTER_SUPERVISOR")
            .env_remove("INVOCATION_ID")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if supervised {
            command
                .env("TRUFFLEPIG_ROUTER_SUPERVISOR", "systemd")
                .env("INVOCATION_ID", "router-binary-restart-test");
        }
        let mut router = Self {
            child: command.spawn().unwrap(),
            runtime,
            daemon_cache: directory.join("root-cache"),
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        while !running(&router.runtime) {
            assert!(Instant::now() < deadline, "router did not bind");
            assert!(router.child.try_wait().unwrap().is_none(), "router exited");
            std::thread::sleep(Duration::from_millis(20));
        }
        router
    }

    fn ask(&self, args: &[String]) -> serde_json::Value {
        let reply = request_by(
            &self.runtime,
            args,
            &RequestContext::new(None, None),
            QueryDeadline::after(Duration::from_secs(20)),
        )
        .unwrap()
        .expect("router answers");
        serde_json::from_str(&reply).unwrap_or(serde_json::Value::String(reply))
    }

    fn lifecycle(&self) -> serde_json::Value {
        self.ask(&["system".into(), "status".into()])["lifecycle"].clone()
    }

    /// Routes a root request so the router spawns and tracks a root daemon.
    fn spawn_root_daemon(&self, root: &Path) {
        let args = [
            "--no-workspace".to_owned(),
            "--root".to_owned(),
            root.display().to_string(),
            "--cache".to_owned(),
            self.daemon_cache.display().to_string(),
            "status".to_owned(),
        ];
        let _ = request_by(
            &self.runtime,
            &args,
            &RequestContext::new(None, None),
            QueryDeadline::after(Duration::from_secs(20)),
        );
        assert!(
            running(&self.daemon_cache),
            "the router spawned a root daemon"
        );
    }

    fn wait_exit(&mut self, within: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().unwrap() {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }
}

impl Drop for PrivateRouter {
    fn drop(&mut self) {
        if running(&self.daemon_cache) {
            let _ = stop(&self.daemon_cache);
        }
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Copies the test binary into `directory`, as `cargo install` places it.
fn install_binary(directory: &Path) -> PathBuf {
    let bin = directory.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let installed = bin.join("trufflepig");
    fs::copy(env!("CARGO_BIN_EXE_trufflepig"), &installed).unwrap();
    installed
}

/// Replaces the binary by rename, leaving the running inode deleted.
fn replace_binary(installed: &Path) {
    let staged = installed.with_extension("staged");
    fs::copy(env!("CARGO_BIN_EXE_trufflepig"), &staged).unwrap();
    fs::rename(&staged, installed).unwrap();
}

#[test]
fn supervised_router_drains_and_exits_for_a_settled_replacement() {
    let directory = scratch();
    let binary = install_binary(directory.path());
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("lib.rs"), "pub fn restart_probe() {}\n").unwrap();
    let mut router = PrivateRouter::start(directory.path(), &binary, true);
    let lifecycle = router.lifecycle();
    assert_eq!(lifecycle["supervisor"], "systemd", "{lifecycle}");
    assert_eq!(lifecycle["binary_restart"], "current", "{lifecycle}");
    router.spawn_root_daemon(&root);

    replace_binary(&binary);
    let status = router
        .wait_exit(Duration::from_secs(60))
        .expect("the supervised router exits after the replacement settles");
    let mut stderr = String::new();
    std::io::Read::read_to_string(router.child.stderr.as_mut().unwrap(), &mut stderr).unwrap();
    assert_eq!(status.code(), Some(RESTART_EXIT_STATUS), "{stderr}");
    assert!(stderr.contains("router_restart: "), "{stderr}");
    assert!(!router.runtime.join("daemon.sock").exists());
    let record = fs::read_to_string(router.runtime.join("router-restarts")).unwrap();
    assert_eq!(record.lines().count(), 1, "{record}");
    assert!(
        !running(&router.daemon_cache),
        "the router stopped the root daemon it spawned"
    );
}

#[test]
fn detached_router_keeps_serving_after_a_replacement() {
    let directory = scratch();
    let binary = install_binary(directory.path());
    let mut router = PrivateRouter::start(directory.path(), &binary, false);
    assert_eq!(router.lifecycle()["supervisor"], "detached");
    replace_binary(&binary);
    assert!(
        router.wait_exit(Duration::from_secs(5)).is_none(),
        "a detached router never exits for a replacement"
    );
    let status = router.ask(&["system".into(), "status".into()]);
    assert_eq!(status["executable_replaced"], true, "{status}");
    stop(&router.runtime).unwrap();
    let status = router
        .wait_exit(Duration::from_secs(40))
        .expect("router stops");
    assert_eq!(status.code(), Some(0));
}

const REPLACED_REFUSAL: &str = "daemon: router_binary_replaced: executable /opt/trufflepig (deleted) for cache /cache/root was deleted after the system router started; restart the owning unit with `systemctl --user restart trufflepig-system.service`";

/// A router stub that refuses every request as `router_binary_replaced`.
struct ReplacedRouterStub {
    stop: Arc<AtomicBool>,
    served: Arc<AtomicUsize>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ReplacedRouterStub {
    fn start(runtime: &Path) -> Self {
        fs::create_dir_all(runtime).unwrap();
        let listener = UnixListener::bind(runtime.join("daemon.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (stop, served) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicUsize::new(0)),
        );
        let (stopping, counting) = (Arc::clone(&stop), Arc::clone(&served));
        let thread = std::thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                if daemon_wire_protocol::read_request(&mut stream).is_ok() {
                    counting.fetch_add(1, Ordering::AcqRel);
                    let reply = DaemonReply::Failure {
                        message: REPLACED_REFUSAL.to_owned(),
                    };
                    let _ = daemon_wire_protocol::write_reply(&mut stream, &reply);
                }
            }
        });
        Self {
            stop,
            served,
            thread: Some(thread),
        }
    }
}

impl Drop for ReplacedRouterStub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[test]
fn reads_refused_for_a_replaced_router_run_locally_and_writes_keep_the_refusal() {
    let directory = scratch();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("lib.rs"), "pub fn local_fallback_probe() {}\n").unwrap();
    let runtime = directory.path().join("runtime");
    let stub = ReplacedRouterStub::start(&runtime);
    let run = |words: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_trufflepig"))
            .args(["--no-workspace", "--root"])
            .arg(&root)
            .arg("--cache")
            .arg(directory.path().join("root-cache"))
            .args(words)
            .env("TRUFFLEPIG_SYSTEM_DIR", &runtime)
            .env("TRUFFLEPIG_SPOOL_DIR", directory.path().join("spool"))
            .env("XDG_CONFIG_HOME", directory.path().join("config"))
            .env("XDG_DATA_HOME", directory.path().join("data"))
            .env("XDG_CACHE_HOME", directory.path().join("cache"))
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };

    let read = run(&["show", "path:lib.rs"]);
    let stdout = String::from_utf8_lossy(&read.stdout);
    let stderr = String::from_utf8_lossy(&read.stderr);
    assert!(read.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("local_fallback_probe"), "{stdout}");
    let after_read = stub.served.load(Ordering::Acquire);
    assert!(after_read >= 1, "the read reached the router first");
    assert!(
        !running(&directory.path().join("root-cache")),
        "the fallback runs with --no-daemon and starts no root daemon"
    );

    let write = run(&["hist-index"]);
    let stderr = String::from_utf8_lossy(&write.stderr);
    assert_eq!(write.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("router_binary_replaced:"), "{stderr}");
    assert!(stderr.contains("systemctl --user restart"), "{stderr}");
    assert!(stub.served.load(Ordering::Acquire) > after_read);
}
