use std::{
    fs,
    io::Read,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::{Duration, Instant},
};

static CHILD_SERIAL: AtomicUsize = AtomicUsize::new(1);
const OUTPUT_BUDGET: &str = "16000";

pub struct Fixture {
    pub temp: tempfile::TempDir,
    pub root: PathBuf,
    pub cache: PathBuf,
    pub system_dir: PathBuf,
    home: PathBuf,
    config: PathBuf,
    xdg_cache: PathBuf,
    data: PathBuf,
    runtime: PathBuf,
    spool: PathBuf,
    board_db: PathBuf,
    temp_files: PathBuf,
}

pub struct ReadOnlyCache {
    path: PathBuf,
    original_mode: u32,
}

pub struct ProcessOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Fixture {
    pub fn new() -> Self {
        let parent = scratch_parent();
        fs::create_dir_all(&parent).unwrap();
        let temp = tempfile::Builder::new()
            .prefix("cp-")
            .tempdir_in(parent)
            .unwrap();
        let root = temp.path().join("project");
        let cache = temp.path().join("readonly-cache");
        let system_dir = temp.path().join("system");
        let home = temp.path().join("home");
        let config = temp.path().join("config");
        let xdg_cache = temp.path().join("xdg-cache");
        let data = temp.path().join("data");
        let runtime = temp.path().join("runtime");
        let spool = temp.path().join("spool");
        let board_db = temp.path().join("board.sqlite3");
        let temp_files = temp.path().join("tmp");
        for directory in [
            &root,
            &cache,
            &system_dir,
            &home,
            &config,
            &xdg_cache,
            &data,
            &runtime,
            &spool,
            &temp_files,
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(cache.join("fixture-marker"), b"preserve-cache\n").unwrap();
        Self {
            temp,
            root,
            cache,
            system_dir,
            home,
            config,
            xdg_cache,
            data,
            runtime,
            spool,
            board_db,
            temp_files,
        }
    }

    pub fn write_markdown(&self, rows: usize) -> Vec<u8> {
        self.write_markdown_with_prefix("guide", rows)
    }

    pub fn write_markdown_with_prefix(&self, prefix: &str, rows: usize) -> Vec<u8> {
        let mut text = String::with_capacity(rows.saturating_mul(prefix.len() + 16));
        for line in 1..=rows {
            use std::fmt::Write;
            writeln!(text, "# {prefix} row-{line:03}").unwrap();
        }
        let docs = self.root.join("docs");
        fs::create_dir_all(&docs).unwrap();
        fs::write(docs.join("guide.md"), text.as_bytes()).unwrap();
        text.into_bytes()
    }

    pub fn write_workspace(&self) -> (PathBuf, PathBuf) {
        let selected = self.temp.path().join("selected");
        fs::create_dir_all(selected.join("docs")).unwrap();
        fs::write(
            selected.join("docs/guide.md"),
            b"# selected member marker\n# selected second row\n",
        )
        .unwrap();
        let workspace = self.temp.path().join("workspace.toml");
        fs::write(
            &workspace,
            "[workspace]\nname=\"path-fixture\"\n[members.home]\npath=\"project\"\n[members.selected]\npath=\"selected\"\n",
        )
        .unwrap();
        (workspace, selected)
    }

    pub fn make_cache_readonly(&self) -> ReadOnlyCache {
        let metadata = fs::metadata(&self.cache).unwrap();
        let original_mode = metadata.permissions().mode();
        fs::set_permissions(&self.cache, fs::Permissions::from_mode(0o500)).unwrap();
        ReadOnlyCache {
            path: self.cache.clone(),
            original_mode,
        }
    }

    pub fn make_regular_cache_file(&self) -> PathBuf {
        let path = self.temp.path().join("c");
        assert_workspace_socket_path_fits(&path);
        fs::write(&path, b"not a cache directory\n").unwrap();
        path
    }

    pub fn cache_snapshot(&self, path: &Path) -> Vec<(String, u32, Option<Vec<u8>>)> {
        let mut entries = Vec::new();
        snapshot(path, Path::new(""), &mut entries);
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries
    }

    pub fn default_cache_snapshot(&self) -> Vec<(String, u32, Option<Vec<u8>>)> {
        self.cache_snapshot(&self.xdg_cache)
    }

    pub fn start_system_server(
        &self,
        cache: &Path,
        timeout: Duration,
    ) -> super::busy_daemon::SystemServer {
        let mut command = self.isolated_command(cache, None, None);
        command.arg("system-serve");
        super::busy_daemon::SystemServer::start(
            &mut command,
            self.system_dir.join("daemon.sock"),
            timeout,
        )
    }

    pub fn run_show(
        &self,
        target: &str,
        cache: &Path,
        no_daemon: bool,
        timeout: Duration,
    ) -> serde_json::Value {
        let output = run_bounded(&mut self.show_command(target, cache, no_daemon), timeout);
        assert!(
            output.status.success(),
            "show {target} failed: {}",
            output.combined_text()
        );
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "show {target} returned invalid JSON: {error}; {}",
                output.combined_text()
            )
        })
    }

    pub fn run_workspace_show(
        &self,
        workspace: &Path,
        target: &str,
        cache: &Path,
        no_daemon: bool,
        timeout: Duration,
    ) -> serde_json::Value {
        let mut command = self.isolated_command(cache, Some(workspace), Some("selected"));
        if no_daemon {
            command.arg("--no-daemon");
        }
        command.args(["show", target]);
        let output = run_bounded(&mut command, timeout);
        assert!(
            output.status.success(),
            "workspace show {target} failed: {}",
            output.combined_text()
        );
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "workspace show {target} returned invalid JSON: {error}; {}",
                output.combined_text()
            )
        })
    }

    pub fn run_show_failure(
        &self,
        target: &str,
        cache: &Path,
        no_daemon: bool,
        timeout: Duration,
    ) -> String {
        let output = run_bounded(&mut self.show_command(target, cache, no_daemon), timeout);
        assert!(
            !output.status.success(),
            "show {target} unexpectedly succeeded: {}",
            output.combined_text()
        );
        output.combined_text()
    }

    fn show_command(&self, target: &str, cache: &Path, no_daemon: bool) -> Command {
        let mut command = self.isolated_command(cache, None, None);
        if no_daemon {
            command.arg("--no-daemon");
        }
        command.args(["show", target]);
        command
    }

    fn isolated_command(
        &self,
        cache: &Path,
        workspace: Option<&Path>,
        member: Option<&str>,
    ) -> Command {
        let child = CHILD_SERIAL.fetch_add(1, Ordering::Relaxed);
        let mut command = Command::new(env!("CARGO_BIN_EXE_trufflepig"));
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("XDG_CACHE_HOME", &self.xdg_cache)
            .env("XDG_DATA_HOME", &self.data)
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("TRUFFLEPIG_SYSTEM_DIR", &self.system_dir)
            .env("TRUFFLEPIG_SPOOL_DIR", &self.spool)
            .env("TRUFFLEPIG_BOARD_DB", &self.board_db)
            .env("TRUFFLEPIG_SESSION", format!("current-path-cli-{child}"))
            .env("TMPDIR", &self.temp_files)
            .current_dir(&self.root);
        if let Some(workspace) = workspace {
            command
                .arg("--workspace")
                .arg(workspace)
                .arg("--member")
                .arg(member.expect("workspace command has selected member"));
        } else {
            command.arg("--no-workspace");
        }
        command.args([
            "--root",
            self.root.to_str().expect("fixture path is UTF-8"),
            "--cache",
            cache.to_str().expect("cache path is UTF-8"),
            "--diagnostics",
            "off",
            "--budget",
            OUTPUT_BUDGET,
        ]);
        command.process_group(0);
        command
    }
}

impl ReadOnlyCache {
    pub fn mode_is_read_only(&self) -> bool {
        fs::metadata(&self.path)
            .map(|metadata| metadata.permissions().mode() & 0o222 == 0)
            .unwrap_or(false)
    }
}

impl Drop for ReadOnlyCache {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(self.original_mode));
    }
}

impl ProcessOutput {
    pub fn combined_text(&self) -> String {
        format!(
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&self.stdout),
            String::from_utf8_lossy(&self.stderr)
        )
    }
}

pub fn run_bounded(command: &mut Command, timeout: Duration) -> ProcessOutput {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn trufflepig CLI");
    let process_group = child.id() as i32;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let stdout_reader = thread::spawn(move || drain(stdout));
    let stderr_reader = thread::spawn(move || drain(stderr));
    let mut child = ChildGuard {
        child: Some(child),
        process_group,
    };
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.child.as_mut().unwrap().try_wait() {
            Ok(Some(status)) => {
                kill_process_group(process_group, libc::SIGKILL);
                child.child.take();
                break status;
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                kill_process_group(process_group, libc::SIGKILL);
                let process = child.child.as_mut().unwrap();
                let _ = process.kill();
                let status = process.wait().expect("reap timed-out CLI child");
                child.child.take();
                let stdout = stdout_reader.join().expect("join stdout drain").unwrap();
                let stderr = stderr_reader.join().expect("join stderr drain").unwrap();
                panic!(
                    "trufflepig CLI exceeded {timeout:?} with {status}: stdout: {}\nstderr: {}",
                    String::from_utf8_lossy(&stdout),
                    String::from_utf8_lossy(&stderr)
                );
            }
            Err(error) => panic!("wait for trufflepig CLI: {error}"),
        }
    };
    ProcessOutput {
        status,
        stdout: stdout_reader.join().expect("join stdout drain").unwrap(),
        stderr: stderr_reader.join().expect("join stderr drain").unwrap(),
    }
}

struct ChildGuard {
    child: Option<Child>,
    process_group: i32,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        kill_process_group(self.process_group, libc::SIGKILL);
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub(super) fn kill_process_group(process_group: i32, signal: i32) {
    // Each fixture command starts in its own group; cleanup cannot reach the host service.
    unsafe {
        libc::kill(-process_group, signal);
    }
}

fn scratch_parent() -> PathBuf {
    let configured = std::env::var_os("TMPDIR").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let codex_tmp = home.map(|home| home.join(".cache/codex-tmp"));
    let parent = match (configured, codex_tmp) {
        (Some(configured), Some(codex_tmp)) if configured.starts_with(&codex_tmp) => codex_tmp,
        (Some(configured), _) => configured,
        (None, Some(codex_tmp)) => codex_tmp,
        (None, None) => panic!("HOME or TMPDIR required for integration scratch"),
    };
    assert!(
        parent.is_absolute() && !parent.starts_with("/tmp"),
        "integration scratch must stay on disk outside /tmp: {}",
        parent.display()
    );
    parent
}

fn assert_workspace_socket_path_fits(cache: &Path) {
    const WORKSPACE_CACHE_ID_LENGTH: usize = 24;
    let socket = cache
        .join("workspace")
        .join("0".repeat(WORKSPACE_CACHE_ID_LENGTH))
        .join("daemon.sock");
    let socket_length = socket.as_os_str().as_bytes().len();
    // SAFETY: sockaddr_un contains only integer fields and a byte array; zero is valid.
    let socket_capacity = unsafe { std::mem::zeroed::<libc::sockaddr_un>() }
        .sun_path
        .len();
    assert!(
        socket_length < socket_capacity,
        "fixture workspace daemon socket path is {socket_length} bytes, but sockaddr_un.sun_path holds fewer than {socket_capacity}: {}",
        socket.display()
    );
}

fn snapshot(root: &Path, relative: &Path, entries: &mut Vec<(String, u32, Option<Vec<u8>>)>) {
    let metadata = fs::symlink_metadata(root).unwrap();
    let name = relative.to_string_lossy().into_owned();
    let mode = metadata.permissions().mode();
    if metadata.is_dir() {
        entries.push((name.clone(), mode, None));
        for entry in fs::read_dir(root).unwrap() {
            let entry = entry.unwrap();
            let child = relative.join(entry.file_name());
            snapshot(&entry.path(), &child, entries);
        }
    } else if metadata.is_file() {
        entries.push((name, mode, Some(fs::read(root).unwrap())));
    } else {
        entries.push((name, mode, None));
    }
}

fn drain(mut pipe: impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)?;
    Ok(bytes)
}
