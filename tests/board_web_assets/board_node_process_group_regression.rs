#![cfg(all(unix, board_node_20))]

use std::{
    env,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

use super::run_node_tests;

pub(crate) fn run_isolated_reaping_probe(test_name: &str) -> bool {
    if env::var_os("TRUFFLEPIG_NODE_REAPING_PROBE").as_deref()
        != Some(std::ffi::OsStr::new(test_name))
    {
        let output = Command::new(env::current_exe().expect("runner test executable"))
            .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
            .env("TRUFFLEPIG_NODE_REAPING_PROBE", test_name)
            .output()
            .expect("isolated process-group probe");
        assert!(
            output.status.success(),
            "process-group probe failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("running 1 test\n"),
            "probe ran no test: {stdout}"
        );
        assert!(stdout.contains("test result: ok. 1 passed; 0 failed; 0 ignored;"));
        return false;
    }

    #[cfg(target_os = "linux")]
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) },
        0,
        "the isolated probe must adopt and reap Node orphans"
    );
    true
}

pub(crate) fn run_process_group_regression(test_name: &str) {
    if !run_isolated_reaping_probe(test_name) {
        return;
    }

    let node = crate::resolve_on_path("node").expect("node on PATH: build probed Node >=20");
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/descendant_open_pipes.test.mjs");
    let error = run_node_tests(&node, &[fixture], Duration::from_secs(3))
        .expect_err("a descendant keeping pipes open must hit the deadline");
    assert!(
        error.contains("node --test timed out after 3 s"),
        "unexpected failure: {error}"
    );
    let group = error
        .split_whitespace()
        .find_map(|word| word.strip_prefix("NODE_PROCESS_GROUP_ID="))
        .and_then(|value| value.parse::<libc::pid_t>().ok())
        .expect("timeout diagnostics identify the owned process group");
    assert!(
        error.contains("DESCENDANT_PID="),
        "descendant did not start"
    );

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        #[cfg(target_os = "linux")]
        while unsafe { libc::waitpid(-group, std::ptr::null_mut(), libc::WNOHANG) } > 0 {}
        let result = unsafe { libc::kill(-group, 0) };
        if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "owned process group {group} still exists after cleanup"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
