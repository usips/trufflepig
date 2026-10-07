use super::*;
use std::{
    fs::{self, File},
    os::unix::{fs::FileTypeExt, thread::JoinHandleExt},
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
};

const CHILD_RUNTIME: &str = "TRUFFLEPIG_BOARD_WEB_SHUTDOWN_RUNTIME";
const CHILD_DEADLINE: Duration = Duration::from_secs(5);
const CHILD_PATH_MARKER: &str = "board-web shutdown child: entering signal waiter";

#[test]
fn fifo_descriptor_does_not_block_exit() {
    if let Some(runtime) = std::env::var_os(CHILD_RUNTIME) {
        exit_through_signal_waiter(Path::new(&runtime), false);
    }
    let directory = crate::board::board_test_support::scratch("web-fifo-exit-");
    let descriptor = directory.path().join("board-web.json");
    let path = std::ffi::CString::new(descriptor.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: mkfifo receives a fresh scratch path and its result is checked.
    assert_eq!(
        unsafe { libc::mkfifo(path.as_ptr(), 0o600) },
        0,
        "mkfifo failed: {}",
        io::Error::last_os_error()
    );
    let status = run_shutdown_child(directory.path(), "fifo_descriptor_does_not_block_exit");
    assert!(
        status.success(),
        "the signal waiter did not exit cleanly: {status}"
    );
    assert!(
        fs::symlink_metadata(descriptor)
            .unwrap()
            .file_type()
            .is_fifo(),
        "shutdown must leave a planted FIFO alone"
    );
}

#[test]
fn signal_exit_removes_the_owned_descriptor() {
    if let Some(runtime) = std::env::var_os(CHILD_RUNTIME) {
        exit_through_signal_waiter(Path::new(&runtime), false);
    }
    let directory = crate::board::board_test_support::scratch("web-owned-exit-");
    let runtime = directory.path();
    super::super::web_endpoint::publish(
        runtime,
        "127.0.0.1:7341".parse().unwrap(),
        &runtime.join("web.sqlite3"),
    )
    .unwrap();
    let status = run_shutdown_child(runtime, "signal_exit_removes_the_owned_descriptor");
    assert!(
        status.success(),
        "the signal waiter did not exit cleanly: {status}"
    );
    assert!(!runtime.join("board-web.json").exists());
}

#[test]
fn signal_exit_holds_the_publish_mutex() {
    if let Some(runtime) = std::env::var_os(CHILD_RUNTIME) {
        exit_through_signal_waiter(Path::new(&runtime), true);
    }
    let directory = crate::board::board_test_support::scratch("web-locked-exit-");
    let status = run_shutdown_child(directory.path(), "signal_exit_holds_the_publish_mutex");
    assert!(
        status.success(),
        "the exit waiter released the publication mutex"
    );
}

fn exit_through_signal_waiter(runtime: &Path, require_exit_lock: bool) -> ! {
    signal_shutdown::block_termination().unwrap();
    let published = Arc::new(if require_exit_lock {
        PublishedEndpoint::checking_exit_lock_for_test()
    } else {
        PublishedEndpoint::default()
    });
    if !require_exit_lock {
        published.record_for_test(
            runtime.join("board-web.json"),
            "127.0.0.1:7341".parse().unwrap(),
        );
    }
    eprintln!("{CHILD_PATH_MARKER}");
    let waiter = signal_shutdown::spawn_exit_waiter(published).unwrap();
    // SAFETY: the handle names our live waiter, which inherited the blocked
    // mask. A thread-directed signal cannot hit the test harness's main thread.
    assert_eq!(
        unsafe { libc::pthread_kill(waiter.as_pthread_t(), libc::SIGTERM) },
        0
    );
    waiter.join().unwrap();
    panic!("the signal waiter returned instead of exiting");
}

fn run_shutdown_child(runtime: &Path, name: &str) -> ExitStatus {
    let output_path = runtime.join("shutdown-child.log");
    let output = File::create(&output_path).unwrap();
    let child = Command::new(std::env::current_exe().unwrap())
        .env(CHILD_RUNTIME, runtime)
        .args([
            &format!("board::board_web::tests::serve_shutdown_tests::{name}"),
            "--exact",
            "--nocapture",
        ])
        .stdout(Stdio::from(output.try_clone().unwrap()))
        .stderr(Stdio::from(output))
        .spawn()
        .unwrap();
    let mut child = ShutdownChild(child);
    let deadline = Instant::now() + CHILD_DEADLINE;
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            let child_log = fs::read_to_string(&output_path).unwrap();
            assert!(
                status.success(),
                "signal child failed with {status}\n{child_log}"
            );
            assert!(
                child_log.lines().any(|line| line == "running 1 test"),
                "signal child did not run exactly one test\n{child_log}"
            );
            assert!(
                child_log
                    .lines()
                    .any(|line| line.ends_with(CHILD_PATH_MARKER)),
                "signal child did not enter the shutdown path\n{child_log}"
            );
            return status;
        }
        if Instant::now() >= deadline {
            child.0.kill().unwrap();
            child.0.wait().unwrap();
            panic!(
                "signal exit blocked on descriptor cleanup for over five seconds\n{}",
                fs::read_to_string(output_path).unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct ShutdownChild(Child);

impl Drop for ShutdownChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
