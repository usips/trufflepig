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
        if daemon::request_by(&runtime, &ping, &context, deadline.clone())
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
    let ensured = ensure_with(ROUTER_START_WAIT, |_| {
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
    let _error = ensure_with(Duration::from_secs(2), start_router).unwrap_err();
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

fn unit_owning(runtime: &std::path::Path) -> String {
    format!(
        "[Service]\nEnvironment=\"TRUFFLEPIG_SYSTEM_DIR={}\"\n",
        runtime.display()
    )
}

#[test]
fn managed_start_waits_for_the_unit_router_without_a_child() {
    if isolated_router_test("managed_start_waits_for_the_unit_router_without_a_child") {
        return;
    }
    let runtime = dir().unwrap();
    let unit = unit_owning(&runtime);
    // The first baseline read fails, which is not a failed start; the router
    // binds 600 ms after the unit start, so the watch reads `activating` first.
    let systemctl = super::managed_unit_tests::FakeSystemctl::new([
        Err("Failed to connect to bus"),
        Ok("ActiveState=activating\nNRestarts=0"),
    ]);
    let worker = std::cell::RefCell::new(None);
    let spawned_before = crate::background_process::spawned_count();
    let ensured = ensure_until(
        QueryDeadline::after(Duration::from_secs(10)),
        Duration::from_secs(10),
        |deadline| {
            managed_unit::start_router_with(
                Some(&unit),
                &runtime,
                &deadline,
                |args| {
                    if args.contains(&"start") {
                        *worker.borrow_mut() = Some(std::thread::spawn(|| {
                            std::thread::sleep(Duration::from_millis(600));
                            serve()
                        }));
                    }
                    systemctl.run(args)
                },
                || panic!("the unit owns this router; no detached spawn"),
            )
        },
    );
    let spawned = crate::background_process::spawned_count() - spawned_before;
    ensured.unwrap();
    assert_eq!(spawned, 0, "a unit-owned start spawns no detached router");
    assert!(
        systemctl.show_count() >= 1,
        "the watch read the unit while it was activating"
    );
    daemon::stop(&runtime).unwrap();
    worker
        .into_inner()
        .expect("the unit start ran the router")
        .join()
        .unwrap()
        .unwrap();
}

#[test]
fn unit_that_fails_at_startup_bails_before_the_deadline() {
    if isolated_router_test("unit_that_fails_at_startup_bails_before_the_deadline") {
        return;
    }
    let runtime = dir().unwrap();
    let unit = unit_owning(&runtime);
    let systemctl = super::managed_unit_tests::FakeSystemctl::new([
        Ok("ActiveState=activating\nNRestarts=0"),
        Ok("ActiveState=failed\nNRestarts=0"),
    ]);
    let started = Instant::now();
    let error = ensure_until(
        QueryDeadline::after(Duration::from_secs(10)),
        Duration::from_secs(10),
        |deadline| {
            managed_unit::start_router_with(
                Some(&unit),
                &runtime,
                &deadline,
                |args| systemctl.run(args),
                || panic!("the unit owns this router; no detached spawn"),
            )
        },
    )
    .unwrap_err();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "a failed unit bails before the 10 s deadline: {elapsed:?}"
    );
    assert_eq!(
        format!("{error:#}"),
        "system_unavailable: trufflepig-system.service did not start (ActiveState=failed, NRestarts=0); see `journalctl --user -u trufflepig-system.service`"
    );
}

#[test]
fn unit_that_keeps_restarting_bails_once_nrestarts_passes_its_baseline() {
    if isolated_router_test("unit_that_keeps_restarting_bails_once_nrestarts_passes_its_baseline") {
        return;
    }
    let runtime = dir().unwrap();
    let unit = unit_owning(&runtime);
    let systemctl = super::managed_unit_tests::FakeSystemctl::new([
        Ok("ActiveState=activating\nNRestarts=0"),
        Ok("ActiveState=activating\nNRestarts=1"),
    ]);
    let started = Instant::now();
    let error = ensure_until(
        QueryDeadline::after(Duration::from_secs(10)),
        Duration::from_secs(10),
        |deadline| {
            managed_unit::start_router_with(
                Some(&unit),
                &runtime,
                &deadline,
                |args| systemctl.run(args),
                || panic!("the unit owns this router; no detached spawn"),
            )
        },
    )
    .unwrap_err();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "a crash-looping unit bails before the 10 s deadline: {elapsed:?}"
    );
    assert!(
        format!("{error:#}").contains("(ActiveState=activating, NRestarts=1)"),
        "{error:#}"
    );
}
