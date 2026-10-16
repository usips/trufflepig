#![cfg(target_os = "linux")]

use rusqlite::{Connection, OpenFlags};
use std::os::unix::fs::PermissionsExt;
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

const REQUESTS: usize = 12;
const STDERR_LIMIT: usize = 16 * 1024;

struct BoardServer {
    child: ManagedChild,
    stderr_reader: Option<JoinHandle<Vec<u8>>>,
    audit_lines: Arc<AtomicUsize>,
}

impl BoardServer {
    fn start(directory: &Path, empty_root: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_trufflepig"))
            .env("TRUFFLEPIG_SYSTEM_DIR", directory.join("runtime"))
            .env("TRUFFLEPIG_SPOOL_DIR", directory.join("spool"))
            .env("TRUFFLEPIG_BOARD_DB", directory.join("board.sqlite3"))
            .env("XDG_CONFIG_HOME", directory.join("config"))
            .env("XDG_DATA_HOME", directory.join("data"))
            .env("XDG_CACHE_HOME", directory.join("cache"))
            .args(["--no-workspace", "--root"])
            .arg(empty_root)
            .args(["board-serve", "--listen", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let audit_lines = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&audit_lines);
        let stderr_reader = std::thread::spawn(move || {
            let mut captured = Vec::with_capacity(STDERR_LIMIT);
            let mut chunk = [0; 1024];
            while let Ok(count) = stderr.read(&mut chunk) {
                if count == 0 {
                    break;
                }
                counted.fetch_add(
                    chunk[..count].iter().filter(|byte| **byte == b'\n').count(),
                    Ordering::Release,
                );
                let remaining = STDERR_LIMIT.saturating_sub(captured.len());
                captured.extend_from_slice(&chunk[..count.min(remaining)]);
            }
            captured
        });
        Self {
            child: ManagedChild(child),
            stderr_reader: Some(stderr_reader),
            audit_lines,
        }
    }

    fn address(&mut self, runtime: &Path) -> SocketAddr {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Ok(bytes) = std::fs::read(runtime.join("board-web.json"))
                && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
                && let Some(address) = value["address"]
                    .as_str()
                    .and_then(|value| value.parse().ok())
            {
                return address;
            }
            if let Some(status) = self.child.0.try_wait().unwrap() {
                self.fail(&format!("board-serve exited during startup: {status}"));
            }
            if Instant::now() >= deadline {
                self.fail("board-serve did not publish an endpoint");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn fail(&mut self, reason: &str) -> ! {
        let stderr = self.finish();
        panic!(
            "{reason}\nbounded board-serve stderr:\n{}",
            String::from_utf8_lossy(&stderr)
        );
    }

    fn stop_child(&mut self) {
        self.child.stop();
    }

    fn finish(&mut self) -> Vec<u8> {
        self.stop_child();
        self.stderr_reader.take().unwrap().join().unwrap()
    }
}

impl Drop for BoardServer {
    fn drop(&mut self) {
        self.stop_child();
    }
}

struct ManagedChild(Child);

impl ManagedChild {
    fn stop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Router(ManagedChild);

impl Router {
    fn start(directory: &Path, empty_root: &Path) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_trufflepig"))
            .env("TRUFFLEPIG_SYSTEM_DIR", directory.join("runtime"))
            .env("TRUFFLEPIG_SPOOL_DIR", directory.join("spool"))
            .env("TRUFFLEPIG_BOARD_DB", directory.join("board.sqlite3"))
            .env("XDG_CONFIG_HOME", directory.join("config"))
            .env("XDG_DATA_HOME", directory.join("data"))
            .env("XDG_CACHE_HOME", directory.join("cache"))
            .args(["--no-workspace", "--root"])
            .arg(empty_root)
            .arg("system-serve")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Self(ManagedChild(child))
    }

    fn wait_for_database(&mut self, database: &Path) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Ok(connection) = Connection::open_with_flags(
                database,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            ) && connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .is_ok_and(|version| version == trufflepig::board::SCHEMA_VERSION)
            {
                return;
            }
            if let Some(status) = self.0.0.try_wait().unwrap() {
                panic!("system-serve exited before database readiness: {status}");
            }
            assert!(
                Instant::now() < deadline,
                "system-serve did not initialize the database"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn scratch() -> tempfile::TempDir {
    let parent = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME or TMPDIR required"))
                .join(".cache/codex-tmp")
        });
    assert!(parent.is_absolute() && !parent.starts_with("/tmp"));
    std::fs::create_dir_all(&parent).unwrap();
    tempfile::Builder::new()
        .prefix("board-web-audit-")
        .tempdir_in(parent)
        .unwrap()
}

fn create_private_directory(path: &Path) {
    std::fs::create_dir(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn get_public_root(address: SocketAddr) -> u16 {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .write_all(
            format!("GET / HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .unwrap();
    let mut response = Vec::with_capacity(4096);
    stream.read_to_end(&mut response).unwrap();
    assert!(response.len() <= 128 * 1024, "response exceeded test bound");
    let headers = std::str::from_utf8(&response)
        .unwrap()
        .split_once("\r\n\r\n")
        .expect("complete HTTP response")
        .0;
    headers
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn board_server_completes_more_requests_than_its_worker_count_and_audits_peers() {
    let directory = scratch();
    let runtime = directory.path().join("runtime");
    create_private_directory(&runtime);
    let empty_root = directory.path().join("empty");
    create_private_directory(&empty_root);
    let database = directory.path().join("board.sqlite3");
    let mut router = Router::start(directory.path(), &empty_root);
    router.wait_for_database(&database);
    let mut server = BoardServer::start(directory.path(), &empty_root);
    let address = server.address(&runtime);
    for index in 0..REQUESTS {
        assert_eq!(get_public_root(address), 200, "request {index}");
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while server.audit_lines.load(Ordering::Acquire) < REQUESTS && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(server.audit_lines.load(Ordering::Acquire) >= REQUESTS);
    let stderr = server.finish();
    assert!(stderr.len() <= STDERR_LIMIT);
    let records = std::str::from_utf8(&stderr)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|record| record["event"] == "board_http")
        .collect::<Vec<_>>();
    assert_eq!(records.len(), REQUESTS);
    for record in records {
        assert_eq!(record["peer_ip"], "127.0.0.1");
        assert_eq!(record["status"], 200);
    }
}
