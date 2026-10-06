//! Run the board web asset tests under the installed Node runtime.
use std::{
    env,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};

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

/// Run `node --test` over `files`, killing the child when `timeout` lapses.
fn run_node_tests(node: &Path, files: &[PathBuf], timeout: Duration) -> Result<Output, String> {
    let mut child = Command::new(node)
        .arg("--test")
        .args(files)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("spawn node --test: {error}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_err(|error| format!("collect node --test output: {error}"));
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "node --test timed out after {} s",
                    timeout.as_secs()
                ));
            }
            Ok(None) => sleep(Duration::from_millis(50)),
            Err(error) => return Err(format!("poll node --test: {error}")),
        }
    }
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
