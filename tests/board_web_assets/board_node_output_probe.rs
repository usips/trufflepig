#![cfg(board_node_20)]

use std::{env, path::PathBuf, process::Command, time::Duration};

use super::run_node_program;

#[test]
fn failed_program_prints_tail_and_every_not_ok() {
    if env::var_os("TRUFFLEPIG_NODE_DIAGNOSTIC_PROBE").is_some() {
        let node = crate::resolve_on_path("node").expect("node on PATH: build probed Node >=20");
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/diagnostic_failure.mjs");
        let output = run_node_program(&node, &fixture, Duration::from_secs(10))
            .expect("nonzero exit still returns Output");
        assert_eq!(output.status.code(), Some(23));
        return;
    }

    let output = Command::new(env::current_exe().expect("runner test executable"))
        .args(["--exact", "board_web_assets::board_node_output_probe::failed_program_prints_tail_and_every_not_ok",
            "--nocapture", "--test-threads=1"])
        .env("TRUFFLEPIG_NODE_DIAGNOSTIC_PROBE", "1")
        .output()
        .expect("nonzero-output diagnostic probe");
    assert!(output.status.success(), "diagnostic probe failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("running 1 test\n"),
        "probe ran no test: {stdout}"
    );
    assert!(stdout.contains("test result: ok. 1 passed; 0 failed; 0 ignored;"));
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    for expected in [
        "not ok 1 - early stdout failure",
        "not ok 2 - early stderr failure",
        "stdout tail 50\n",
        "stderr tail 50\n",
        "stdout tail 249\n",
        "stderr tail 249\n",
    ] {
        assert!(
            diagnostics.contains(expected),
            "missing {expected:?}: {diagnostics}"
        );
    }
    for excluded in [
        "STDOUT_TOO_OLD",
        "STDERR_TOO_OLD",
        "stdout tail 49\n",
        "stderr tail 49\n",
    ] {
        assert!(
            !diagnostics.contains(excluded),
            "old output retained: {excluded}"
        );
    }
}
