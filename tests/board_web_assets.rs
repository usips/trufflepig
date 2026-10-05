//! Run the board web asset tests under the installed Node runtime.
use std::{env, path::PathBuf, process::Command};

/// Resolve `name` to an executable file via `PATH` search.
fn resolve_on_path(name: &str) -> Option<PathBuf> {
    for dir in env::split_paths(&env::var_os("PATH")?) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let executable = dir.join(format!("{name}.exe"));
            if executable.is_file() {
                return Some(executable);
            }
        }
    }
    None
}

#[test]
#[cfg_attr(not(board_node_20), ignore = "requires Node >=20")]
fn board_asset_tests_pass_under_node() {
    let tests =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/board/board_web/assets/tests");
    // Node >=21 treats a `--test` directory argument as a single module and
    // dies with MODULE_NOT_FOUND (nodejs/node#64555); explicit files run
    // identically on every Node >=20.
    let mut files: Vec<PathBuf> = std::fs::read_dir(&tests)
        .unwrap_or_else(|error| panic!("board asset tests unreadable: {}: {error}", tests.display()))
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
    let node = resolve_on_path("node").expect("node on PATH: build probed Node >=20");
    let output = Command::new(node)
        .arg("--test")
        .args(&files)
        .output()
        .expect("spawn node --test");
    println!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        output.status.success(),
        "node --test {}/*.test.mjs failed: {}",
        tests.display(),
        output.status
    );
}
