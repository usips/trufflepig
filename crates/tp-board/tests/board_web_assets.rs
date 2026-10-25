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

    #[cfg(unix)]
    pub(super) use self::board_node_process_group_regression::{
        run_isolated_reaping_probe, run_process_group_regression,
    };
    #[cfg(unix)]
    pub(super) use self::board_node_temporary_files::node_temporary_directory;
    pub(super) use self::board_web_asset_node_runner::{run_node_program, run_node_tests};
}

#[cfg(unix)]
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

/// The `node` on `PATH` when it is Node >=20. Otherwise prints why `test`
/// skips; the build never probes tools on `PATH`.
fn node_20(test: &str) -> Option<PathBuf> {
    let node = resolve_on_path("node");
    let major = node
        .as_ref()
        .and_then(|node| {
            std::process::Command::new(node)
                .arg("--version")
                .output()
                .ok()
        })
        .filter(|output| output.status.success())
        .and_then(|output| node_major(&output.stdout));
    match (node, major) {
        (Some(node), Some(major)) if major >= 20 => Some(node),
        (_, major) => {
            let found = major.map_or("none".to_owned(), |major| format!("v{major}"));
            eprintln!("skipped {test}: requires Node >=20 (found {found})");
            None
        }
    }
}

/// Parses `node --version` stdout into its major version.
fn node_major(bytes: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(bytes).ok()?;
    text.trim()
        .strip_prefix('v')?
        .split('.')
        .next()?
        .parse()
        .ok()
}

#[test]
fn node_version_parsing_requires_a_v_prefixed_major() {
    for major in [20, 21, 22] {
        assert_eq!(
            node_major(format!("v{major}.1.0\n").as_bytes()),
            Some(major)
        );
    }
    assert_eq!(node_major(b"22.1.0"), None);
    assert_eq!(node_major(&[0xff]), None);
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
fn board_asset_tests_pass_under_node() {
    let Some(node) = node_20("board_asset_tests_pass_under_node") else {
        return;
    };
    let files = asset_test_files();
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
fn leaked_timer_fails_instead_of_hanging() {
    let Some(node) = node_20("leaked_timer_fails_instead_of_hanging") else {
        return;
    };
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
fn chatty_suite_passes_within_deadline() {
    let Some(node) = node_20("chatty_suite_passes_within_deadline") else {
        return;
    };
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
fn noisy_node_failure_preserves_both_streams_and_diagnostic() {
    let Some(node) = node_20("noisy_node_failure_preserves_both_streams_and_diagnostic") else {
        return;
    };
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
#[cfg(unix)]
fn timeout_reaps_the_process_group() {
    if node_20("timeout_reaps_the_process_group").is_none() {
        return;
    }
    run_process_group_regression("timeout_reaps_the_process_group");
}

#[test]
#[cfg(unix)]
fn timeout_bounds_escaped_descendant_pipe_wait() {
    if node_20("timeout_bounds_escaped_descendant_pipe_wait").is_none() {
        return;
    }
    if !run_isolated_reaping_probe("timeout_bounds_escaped_descendant_pipe_wait") {
        return;
    }
    let node = resolve_on_path("node").expect("node on PATH: the test checked Node >=20");
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

#[cfg(unix)]
fn unix_process_is_absent(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}
