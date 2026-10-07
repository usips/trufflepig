//! Run the board web asset tests under the installed Node runtime.
use std::{
    env,
    path::{Path, PathBuf},
    time::Duration,
};

mod board_web_assets {
    mod board_web_asset_node_runner;

    pub(super) use self::board_web_asset_node_runner::{run_node_program, run_node_tests};
}

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

/// Sorted `*.test.mjs` files directly inside the board asset test directory.
fn asset_test_files() -> Vec<PathBuf> {
    let tests = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/board/board_web/assets/tests");
    // Node >=21 treats a `--test` directory argument as a single module and
    // dies with MODULE_NOT_FOUND (nodejs/node#64555); explicit files run
    // identically on every Node >=20.
    let mut files: Vec<PathBuf> = std::fs::read_dir(&tests)
        .unwrap_or_else(|error| {
            panic!("board asset tests unreadable: {}: {error}", tests.display())
        })
        .map(|entry| entry.expect("read board asset test entry").path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".test.mjs"))
        })
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "board asset tests missing: {}",
        tests.display()
    );
    files
}

#[test]
#[cfg_attr(not(board_node_20), ignore = "requires Node >=20")]
fn board_asset_tests_pass_under_node() {
    let files = asset_test_files();
    let node = resolve_on_path("node").expect("node on PATH: build probed Node >=20");
    let output =
        run_node_tests(&node, &files, NODE_TEST_TIMEOUT).unwrap_or_else(|error| panic!("{error}"));
    println!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
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
fn noisy_node_success_drains_both_pipes() {
    let node = resolve_on_path("node").expect("node on PATH: build probed Node >=20");
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/noisy_pass.mjs");
    let output = run_node_program(&node, &fixture, Duration::from_secs(30))
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
fn timeout_kills_node_descendants_holding_pipes() {
    let node = resolve_on_path("node").expect("node on PATH: build probed Node >=20");
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/descendant_open_pipes.test.mjs");
    let error = run_node_tests(&node, &[fixture], Duration::from_secs(3))
        .expect_err("a descendant keeping the pipes open must hit the deadline");
    assert!(
        error.contains("node --test timed out after 3 s"),
        "unexpected failure: {error}"
    );
    let pid = error
        .split_whitespace()
        .find_map(|word| word.strip_prefix("DESCENDANT_PID="))
        .and_then(|value| value.parse::<u32>().ok())
        .expect("timeout diagnostics include the descendant PID");
    assert!(
        !unix_process_is_running(pid),
        "process-group descendant {pid} survived timeout cleanup"
    );
}

#[cfg(all(unix, board_node_20))]
fn unix_process_is_running(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_some_and(|state| state != "Z" && state != "X"),
        Err(_) => {
            let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
            result == 0
                || std::io::Error::last_os_error().kind() == std::io::ErrorKind::PermissionDenied
        }
    }
}
