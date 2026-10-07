use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn isolated_router_test(name: &str) -> bool {
    const MARKER: &str = "TRUFFLEPIG_SYSTEM_TEST_SPAWN_CASE";
    if std::env::var(MARKER).as_deref() == Ok(name) {
        return false;
    }
    let directory = crate::board::board_test_support::scratch("router-spawn-case-");
    let test = format!("system::tests::router_spawn_tests::{name}");
    let output = Command::new(std::env::current_exe().unwrap())
        .env(MARKER, name)
        .env("TRUFFLEPIG_SYSTEM_DIR", directory.path().join("runtime"))
        .env("TRUFFLEPIG_SPOOL_DIR", directory.path().join("spool"))
        .env(
            "TRUFFLEPIG_BOARD_DB",
            directory.path().join("board.sqlite3"),
        )
        .env("XDG_CONFIG_HOME", directory.path().join("config"))
        .env("XDG_DATA_HOME", directory.path().join("data"))
        .env("XDG_CACHE_HOME", directory.path().join("cache"))
        .args([test.as_str(), "--exact", "--nocapture"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("{stdout}");
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        stdout.contains("running 1 test"),
        "isolated test selector missed {name}"
    );
    assert!(
        output.status.success(),
        "isolated router test {name} failed"
    );
    true
}

#[test]
fn running_router_answers_without_the_long_wait() {
    if isolated_router_test("running_router_answers_without_the_long_wait") {
        return;
    }
    let runtime = dir().unwrap();
    let worker = std::thread::spawn(serve);
    let ping = ["system".into(), "status".into()];
    let context = RequestContext::new(None, None);
    let deadline = QueryDeadline::after(Duration::from_secs(3));
    loop {
        if daemon::request_by(&runtime, &ping, &context, deadline)
            .unwrap()
            .is_some()
        {
            break;
        }
        assert!(!deadline.expired(), "router did not become ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    let spawned_before = crate::background_process::spawned_count();
    let started = Instant::now();
    let ensured = ensure_with(ROUTER_START_WAIT, || {
        panic!("an answering router needs no spawn")
    });
    let elapsed = started.elapsed();
    daemon::stop(&runtime).unwrap();
    worker.join().unwrap().unwrap();
    ensured.unwrap();
    assert!(
        elapsed < Duration::from_secs(1),
        "status probe took {elapsed:?}"
    );
    assert_eq!(crate::background_process::spawned_count(), spawned_before);
}

#[test]
fn initial_status_probe_leaves_time_for_spawn() {
    if isolated_router_test("initial_status_probe_leaves_time_for_spawn") {
        return;
    }
    // A fresh but undrained spool must not consume the whole startup cap.
    let _spool = daemon::spool::SpoolServer::open(&spool_dir()).unwrap();
    let spawned_before = crate::background_process::spawned_count();
    let _error = ensure_with(Duration::from_secs(2), spawn_router).unwrap_err();
    assert_eq!(
        crate::background_process::spawned_count() - spawned_before,
        1,
        "the initial status probe must leave time to attempt the spawn"
    );
}

#[test]
fn status_probe_uses_spool_without_a_runtime_directory() {
    let directory = crate::board::board_test_support::scratch("router-spool-only-");
    let spool = directory.path().join("spool");
    let mut server = daemon::spool::SpoolServer::open(&spool).unwrap();
    let stopped = Arc::new(AtomicBool::new(false));
    let worker_stopped = Arc::clone(&stopped);
    let worker = std::thread::spawn(move || {
        while !worker_stopped.load(Ordering::Acquire) {
            for request in server.claim() {
                request.answer(|_, args| {
                    assert_eq!(args, ["system", "status"]);
                    Ok("spool router is ready".into())
                });
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    let reply = poll_at(
        None,
        &spool,
        &["system".into(), "status".into()],
        &RequestContext::new(None, None),
        QueryDeadline::after(Duration::from_secs(2)),
    );
    stopped.store(true, Ordering::Release);
    worker.join().unwrap();
    assert_eq!(reply.unwrap().as_deref(), Some("spool router is ready"));
}
