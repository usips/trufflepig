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
    assert!(
        tests.is_dir(),
        "board asset tests missing: {}",
        tests.display()
    );
    let node = resolve_on_path("node").expect("node on PATH: build probed Node >=20");
    let output = Command::new(node)
        .arg("--test")
        .arg(&tests)
        .output()
        .expect("spawn node --test");
    println!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        output.status.success(),
        "node --test {} failed: {}",
        tests.display(),
        output.status
    );
}
