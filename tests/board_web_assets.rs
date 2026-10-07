//! Run the board web asset tests under the installed Node runtime.
use std::{
    env,
    path::{Path, PathBuf},
    time::Duration,
};

mod board_web_assets {
    mod board_node_output_probe;
    mod board_node_process_group_regression;
    mod board_node_temporary_files;
    mod board_web_asset_node_runner;

    #[cfg(all(unix, board_node_20))]
    pub(super) use self::board_node_process_group_regression::{
        run_isolated_reaping_probe, run_process_group_regression,
    };
    #[cfg(all(unix, board_node_20))]
    pub(super) use self::board_node_temporary_files::node_temporary_directory;
    pub(super) use self::board_web_asset_node_runner::{run_node_program, run_node_tests};
}

#[cfg(all(unix, board_node_20))]
use board_web_assets::{
    node_temporary_directory, run_isolated_reaping_probe, run_process_group_regression,
};
use board_web_assets::{run_node_program, run_node_tests};

/// Bound one `node --test` run so a leaked handle fails instead of hanging.
const NODE_TEST_TIMEOUT: Duration = Duration::from_secs(300);

/// True when `path` is a file with at least one executable bit set.
fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Resolve `name` to an executable file via `PATH` search.
fn resolve_on_path(name: &str) -> Option<PathBuf> {
    for dir in env::split_paths(&env::var_os("PATH")?) {
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let executable = dir.join(format!("{name}.exe"));
            if is_executable(&executable) {
                return Some(executable);
            }
        }
    }
    None
}

/// Sorted `*.test.mjs` files beneath the board asset test directory.
fn asset_test_files() -> Vec<PathBuf> {
    let tests = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/board/board_web/assets/tests");
    // Node >=21 treats a `--test` directory argument as a single module and
    // dies with MODULE_NOT_FOUND (nodejs/node#64555); explicit files run
    // identically on every Node >=20.
    let mut files = Vec::new();
    collect_asset_test_files(&tests, &mut files);
    files.sort();
    assert!(
        !files.is_empty(),
        "board asset tests missing: {}",
        tests.display()
    );
    files
}

fn collect_asset_test_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(directory).unwrap_or_else(|error| {
        panic!(
            "board asset tests unreadable: {}: {error}",
            directory.display()
        )
    });
    for entry in entries {
        let path = entry.expect("read board asset test entry").path();
        if path.is_dir() {
            collect_asset_test_files(&path, files);
        } else if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".test.mjs"))
        {
            files.push(path);
        }
    }
}

#[test]
#[cfg_attr(not(board_node_20), ignore = "requires Node >=20")]
fn board_asset_tests_pass_under_node() {
    let files = asset_test_files();
    let node = resolve_on_path("node").expect("node on PATH: build probed Node >=20");
    let output =
        run_node_tests(&node, &files, NODE_TEST_TIMEOUT).unwrap_or_else(|error| panic!("{error}"));
    if output.status.success() {
        println!("{}", String::from_utf8_lossy(&output.stdout));
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
    }
    assert!(
        output.status.success(),
        "node --test failed: {}",
        output.status
    );
}

#[test]
#[cfg_attr(not(board_node_20), ignore = "requires Node >=20")]
fn leaked_timer_fails_instead_of_hanging() {
    let node = resolve_on_path("node").expect("node on PATH: build probed Node >=20");
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/leaked_timer.test.mjs");
    let error = run_node_tests(&node, &[fixture], Duration::from_secs(3))
        .expect_err("a leaked interval keeps the node event loop alive");
    assert!(
        error.contains("node --test timed out after 3 s"),
        "unexpected failure: {error}"
    );
}

#[test]
#[cfg_attr(not(board_node_20), ignore = "requires Node >=20")]
fn chatty_suite_passes_within_deadline() {
    let node = resolve_on_path("node").expect("node on PATH: build probed Node >=20");
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/noisy_pass.mjs");
    let output = run_node_program(&node, &fixture, Duration::from_secs(10))
        .expect("noisy passing Node test completes");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "Node exited with {}",
        output.status
    );
    assert!(output.stdout.len() > 64 * 1024, "stdout was not drained");
    assert!(output.stderr.len() > 64 * 1024, "stderr was not drained");
    assert!(stdout.contains("NOISY_PASS_STDOUT_BEGIN"));
    assert!(stderr.contains("NOISY_PASS_STDERR_BEGIN"));
    assert!(stdout.contains("NOISY_PASS_EXIT_OK"));
}

#[test]
#[cfg_attr(not(board_node_20), ignore = "requires Node >=20")]
fn noisy_node_failure_preserves_both_streams_and_diagnostic() {
    let node = resolve_on_path("node").expect("node on PATH: build probed Node >=20");
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/noisy_failure.mjs");
    let output = run_node_program(&node, &fixture, Duration::from_secs(30))
        .expect("a nonzero Node exit returns captured output");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "Node unexpectedly passed");
    assert!(output.stdout.len() > 64 * 1024, "stdout was not drained");
    assert!(output.stderr.len() > 64 * 1024, "stderr was not drained");
    assert!(stdout.contains("NOISY_FAILURE_STDOUT_BEGIN"));
    assert!(stderr.contains("NOISY_FAILURE_STDERR_BEGIN"));
    assert!(stderr.contains("NOISY_FAILURE_USEFUL_DIAGNOSTIC"));
    assert_eq!(output.status.code(), Some(23));
}

#[test]
#[cfg(all(unix, board_node_20))]
fn timeout_reaps_the_process_group() {
    run_process_group_regression("timeout_reaps_the_process_group");
}

#[test]
#[cfg(all(unix, board_node_20))]
fn timeout_bounds_escaped_descendant_pipe_wait() {
    if !run_isolated_reaping_probe("timeout_bounds_escaped_descendant_pipe_wait") {
        return;
    }
    let node = resolve_on_path("node").expect("node on PATH: build probed Node >=20");
    let directory =
        tempfile::tempdir_in(node_temporary_directory().expect("disk scratch directory"))
            .expect("escaped-pipe fixture directory");
    let program = directory.path().join("escaped_pipe_holder.mjs");
    let pid_file = directory.path().join("escaped-pipe.pid");
    std::fs::write(&program, include_str!("fixtures/escaped_pipe_holder.mjs"))
        .expect("write escaped-pipe fixture");

    std::thread::scope(|scope| {
        let (cleanup, cleanup_request) = std::sync::mpsc::channel();
        let guardian = scope.spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            let pid = loop {
                if let Ok(pid) = std::fs::read_to_string(&pid_file)
                    && let Ok(pid) = pid.parse::<u32>()
                {
                    break pid;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "escaped descendant did not publish its PID"
                );
                std::thread::sleep(Duration::from_millis(10));
            };
            let _ = cleanup_request.recv_timeout(Duration::from_secs(3));
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
            let deadline = std::time::Instant::now() + Duration::from_secs(1);
            loop {
                #[cfg(target_os = "linux")]
                unsafe {
                    libc::waitpid(pid as libc::pid_t, std::ptr::null_mut(), libc::WNOHANG);
                }
                if unix_process_is_absent(pid) {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "escaped fixture {pid} was not reaped"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            pid
        });

        let start = std::time::Instant::now();
        let error = run_node_program(&node, &program, Duration::from_millis(500))
            .expect_err("an escaped pipe holder must hit the deadline");
        let elapsed = start.elapsed();
        let _ = cleanup.send(());
        let pid = guardian.join().expect("escaped descendant cleanup");
        assert!(
            error.contains("node timed out"),
            "unexpected error: {error}"
        );
        assert!(error.contains("ESCAPED_PIPE_PID="));
        assert!(
            elapsed < Duration::from_secs(2),
            "reader join exceeded the post-kill bound: {elapsed:?}"
        );
        assert!(
            unix_process_is_absent(pid),
            "escaped fixture was not reaped"
        );
    });
}

#[cfg(all(unix, board_node_20))]
fn unix_process_is_absent(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}
