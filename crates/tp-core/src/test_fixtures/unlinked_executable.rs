//! Runs one test entry from a copied test binary that is unlinked while it
//! runs, so `/proc/self/exe` names a deleted file inside the child.
use std::{
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const CACHE: &str = "TRUFFLEPIG_TEST_REPLACED_EXECUTABLE_CACHE";
const ROOT: &str = "TRUFFLEPIG_TEST_REPLACED_EXECUTABLE_ROOT";
const READY: &str = "TRUFFLEPIG_TEST_REPLACED_EXECUTABLE_READY";
const RELEASE: &str = "TRUFFLEPIG_TEST_REPLACED_EXECUTABLE_RELEASE";

/// Paths handed to the child: a cache that does not exist yet and an empty root.
pub(crate) struct UnlinkedChild {
    pub(crate) cache: PathBuf,
    pub(crate) root: PathBuf,
}

/// Runs `child_test` (its full test path) in a copy of this test binary,
/// unlinks the copy once the child is ready, and asserts the child passed.
pub(crate) fn run_unlinked(child_test: &str) {
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("copied-test-binary");
    std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    std::fs::set_permissions(&executable, permissions).unwrap();
    let root = directory.path().join("repository-root");
    std::fs::create_dir(&root).unwrap();
    let ready = directory.path().join("child-ready");
    let release = directory.path().join("child-release");

    let mut command = Command::new(&executable);
    command
        .args(["--exact", child_test, "--nocapture"])
        .env(CACHE, directory.path().join("root-cache"))
        .env(ROOT, &root)
        .env(READY, &ready)
        .env(RELEASE, &release)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Parallel tests may fork while fs::copy's destination is writable, inheriting its descriptor.
    // That keeps the copied inode busy until the forked process execs and closes the descriptor.
    let spawn_deadline = Instant::now() + Duration::from_secs(1);
    let mut child = loop {
        match command.spawn() {
            Ok(child) => break child,
            Err(error) if error.raw_os_error() == Some(libc::ETXTBSY) => {
                let remaining = spawn_deadline.saturating_duration_since(Instant::now());
                assert!(
                    !remaining.is_zero(),
                    "copied test executable remained ETXTBSY for 1 second: {error}"
                );
                std::thread::sleep(Duration::from_millis(10).min(remaining));
            }
            Err(error) => panic!("failed to spawn copied test process: {error}"),
        }
    };

    let ready_deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() && Instant::now() < ready_deadline && child.try_wait().unwrap().is_none()
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    if !ready.exists() {
        let _ = child.kill();
        let output = child.wait_with_output().unwrap();
        panic!("copied test process did not become ready: {output:?}");
    }

    std::fs::remove_file(&executable).unwrap();
    std::fs::write(&release, b"continue").unwrap();
    let exit_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < exit_deadline && child.try_wait().unwrap().is_none() {
        std::thread::sleep(Duration::from_millis(10));
    }
    if child.try_wait().unwrap().is_none() {
        let _ = child.kill();
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
}

/// In the child started by `run_unlinked`, waits until the parent unlinked the
/// executable and returns the child paths; returns `None` in a normal test run.
pub(crate) fn unlinked_child() -> Option<UnlinkedChild> {
    let (Some(cache), Some(root), Some(ready), Some(release)) = (
        std::env::var_os(CACHE),
        std::env::var_os(ROOT),
        std::env::var_os(READY),
        std::env::var_os(RELEASE),
    ) else {
        return None;
    };
    let release = PathBuf::from(release);
    std::fs::write(PathBuf::from(ready), b"ready").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !release.exists() {
        assert!(Instant::now() < deadline, "parent did not release child");
        std::thread::sleep(Duration::from_millis(10));
    }
    Some(UnlinkedChild {
        cache: PathBuf::from(cache),
        root: PathBuf::from(root),
    })
}
