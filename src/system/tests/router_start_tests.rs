use super::*;

#[test]
fn ensure_reports_a_running_router_despite_unresolvable_board_database() {
    if std::env::var_os("TRUFFLEPIG_SYSTEM_TEST_RELATIVE_DB").is_none() {
        // The child process owns the board environment; parallel tests never see it.
        let directory = crate::board::board_test_support::scratch("board-lazy-");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .env("TRUFFLEPIG_SYSTEM_TEST_RELATIVE_DB", "1")
            .env("TRUFFLEPIG_BOARD_DB", "relative.sqlite3")
            .env("TRUFFLEPIG_SYSTEM_DIR", directory.path().join("runtime"))
            .env("TRUFFLEPIG_SPOOL_DIR", directory.path().join("spool"))
            .args([
                "system::tests::router_start_tests::ensure_reports_a_running_router_despite_unresolvable_board_database",
                "--exact",
                "--nocapture",
            ])
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let runtime = PathBuf::from(std::env::var_os("TRUFFLEPIG_SYSTEM_DIR").unwrap());
    let spool = PathBuf::from(std::env::var_os("TRUFFLEPIG_SPOOL_DIR").unwrap());
    let router = SystemRouter {
        runtime: Some(runtime.clone()),
        cache_base: None,
        sweeps: Mutex::new(SweepClock::default()),
        board: crate::board::BoardHost::default(),
    };
    let worker_runtime = runtime.clone();
    let worker = std::thread::spawn(move || daemon::serve_router(&worker_runtime, &spool, router));
    let ping: Vec<String> = ["system".into(), "status".into()].into();
    let context = RequestContext::new(None, None);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if daemon::request(&runtime, &ping, &context)
            .unwrap()
            .is_some()
        {
            break;
        }
        assert!(Instant::now() < deadline, "router failed to start");
        std::thread::sleep(Duration::from_millis(10));
    }
    let reply = crate::system::request(&ping, &context).unwrap().unwrap();
    let status: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(status["status"], "ok");
    assert_eq!(status["board_api"], crate::board::BOARD_API);
    assert_eq!(status["schema_supported"], crate::board::SCHEMA_VERSION);
    assert!(status["board_db"].is_null());
    assert!(status["schema_file"].is_null());
    assert!(
        status["board_error"]
            .as_str()
            .is_some_and(|error| error.contains("board database path must be absolute")),
        "{status}"
    );
    crate::system::ensure().unwrap();
    let error = crate::system::request(&["board".into(), "show".into()], &context).unwrap_err();
    assert!(
        format!("{error:#}").contains("board database path must be absolute"),
        "{error:#}"
    );
    daemon::stop(&runtime).unwrap();
    worker.join().unwrap().unwrap();
}

#[test]
fn ensure_waits_for_a_migrating_router_without_spawning_a_second() {
    const MARKER: &str = "TRUFFLEPIG_SYSTEM_TEST_MIGRATING_ROUTER";
    const STEP_DELAY_MS: &str = "TRUFFLEPIG_SYSTEM_TEST_MIGRATION_STEP_DELAY_MS";
    const TEST: &str = "system::tests::router_start_tests::ensure_waits_for_a_migrating_router_without_spawning_a_second";
    if std::env::var_os(MARKER).is_none() {
        // The child process owns the router environment; parallel tests never see it.
        let directory = crate::board::board_test_support::scratch("router-migration-");
        let database = directory.path().join("board.sqlite3");
        crate::board::local_board::seed_storage_schema(&database, crate::board::SCHEMA_VERSION - 1)
            .unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .env(MARKER, "1")
            .env("TRUFFLEPIG_SYSTEM_DIR", directory.path().join("runtime"))
            .env("TRUFFLEPIG_SPOOL_DIR", directory.path().join("spool"))
            .env("TRUFFLEPIG_BOARD_DB", &database)
            .env("XDG_CONFIG_HOME", directory.path().join("config"))
            .env("XDG_DATA_HOME", directory.path().join("data"))
            .env("XDG_CACHE_HOME", directory.path().join("cache"))
            .env(STEP_DELAY_MS, "5000")
            .args([TEST, "--exact", "--nocapture"])
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let runtime = crate::system::dir().expect("child sets TRUFFLEPIG_SYSTEM_DIR");
    let spawned_before = crate::background_process::spawned_count();
    let worker = std::thread::spawn(crate::system::serve);
    // The slowed migration holds the router for five seconds; the socket binds
    // long before it finishes only when the bind precedes migration.
    let bound_early = {
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut bound = false;
        while !bound && Instant::now() < deadline {
            bound = daemon::running(&runtime);
            if !bound {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        bound
    };
    let ensured = crate::system::ensure();
    let spawned = crate::background_process::spawned_count() - spawned_before;
    assert!(ensured.is_ok(), "ensure waits out migration: {ensured:?}");
    assert!(bound_early, "the router binds its socket before migrating");
    assert_eq!(spawned, 0, "a waiting ensure spawns no second router");
    let ping = vec!["system".to_owned(), "status".to_owned()];
    let reply = crate::system::request(&ping, &RequestContext::new(None, None))
        .unwrap()
        .expect("router answers status after migration");
    let status: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(status["schema_supported"], crate::board::SCHEMA_VERSION);
    assert_eq!(status["schema_file"], crate::board::SCHEMA_VERSION);
    daemon::stop(&runtime).unwrap();
    worker.join().unwrap().unwrap();
}

#[test]
fn ensure_waits_for_a_slow_spawned_router() {
    const MARKER: &str = "TRUFFLEPIG_SYSTEM_TEST_SLOW_ROUTER";
    const SERVER: &str = "TRUFFLEPIG_SYSTEM_TEST_SLOW_ROUTER_SERVER";
    const STEP_DELAY_MS: &str = "TRUFFLEPIG_SYSTEM_TEST_MIGRATION_STEP_DELAY_MS";
    const TEST: &str = "system::tests::router_start_tests::ensure_waits_for_a_slow_spawned_router";
    if std::env::var_os(SERVER).is_some() {
        crate::system::serve().unwrap();
        return;
    }
    if std::env::var_os(MARKER).is_none() {
        // The child process owns the router environment; parallel tests never see it.
        let directory = crate::board::board_test_support::scratch("router-slow-spawn-");
        let database = directory.path().join("board.sqlite3");
        crate::board::local_board::seed_storage_schema(&database, crate::board::SCHEMA_VERSION - 1)
            .unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .env(MARKER, "1")
            .env("TRUFFLEPIG_SYSTEM_DIR", directory.path().join("runtime"))
            .env("TRUFFLEPIG_SPOOL_DIR", directory.path().join("spool"))
            .env("TRUFFLEPIG_BOARD_DB", &database)
            .env("XDG_CONFIG_HOME", directory.path().join("config"))
            .env("XDG_DATA_HOME", directory.path().join("data"))
            .env("XDG_CACHE_HOME", directory.path().join("cache"))
            .env(STEP_DELAY_MS, "3000")
            .args([TEST, "--exact", "--nocapture"])
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let runtime = crate::system::dir().expect("child sets TRUFFLEPIG_SYSTEM_DIR");
    let process = SlowRouterProcess(std::cell::Cell::new(None));
    let spawned_before = crate::background_process::spawned_count();
    let started = Instant::now();
    super::ensure_with(Duration::from_secs(10), || {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .env(SERVER, "1")
            .args([TEST, "--exact", "--nocapture"]);
        let child = spawn_background(&mut command)?;
        process.0.set(Some(child.id()));
        Ok(child)
    })
    .unwrap();
    assert_eq!(
        crate::background_process::spawned_count() - spawned_before,
        1
    );
    assert!(
        started.elapsed() >= Duration::from_secs(3),
        "ensure waits for spawned migration"
    );
    daemon::stop(&runtime).unwrap();
    drop(process);
}

struct SlowRouterProcess(std::cell::Cell<Option<u32>>);

impl Drop for SlowRouterProcess {
    fn drop(&mut self) {
        if let Some(pid) = self.0.get() {
            // The background spawner's waiter reaps this one test-owned child.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
    }
}

#[test]
fn ensure_gives_up_after_the_cap() {
    const MARKER: &str = "TRUFFLEPIG_SYSTEM_TEST_HUNG_ROUTER";
    const STEP_DELAY_MS: &str = "TRUFFLEPIG_SYSTEM_TEST_MIGRATION_STEP_DELAY_MS";
    const TEST: &str = "system::tests::router_start_tests::ensure_gives_up_after_the_cap";
    if std::env::var_os(MARKER).is_none() {
        // The child process owns the router environment; parallel tests never see it.
        let directory = crate::board::board_test_support::scratch("router-hung-spawn-");
        let database = directory.path().join("board.sqlite3");
        crate::board::local_board::seed_storage_schema(&database, crate::board::SCHEMA_VERSION - 1)
            .unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .env(MARKER, "1")
            .env("TRUFFLEPIG_SYSTEM_DIR", directory.path().join("runtime"))
            .env("TRUFFLEPIG_SPOOL_DIR", directory.path().join("spool"))
            .env("TRUFFLEPIG_BOARD_DB", &database)
            .env("XDG_CONFIG_HOME", directory.path().join("config"))
            .env("XDG_DATA_HOME", directory.path().join("data"))
            .env("XDG_CACHE_HOME", directory.path().join("cache"))
            .env(STEP_DELAY_MS, "60000")
            .args([TEST, "--exact", "--nocapture"])
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    // The slowed migration holds the router for a minute, well past the cap;
    // the child process exits with the migration thread still sleeping.
    let _worker = std::thread::spawn(crate::system::serve);
    let started = Instant::now();
    let error = super::ensure_with(Duration::from_millis(300), super::spawn_router).unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("system_unavailable: router still starting after"),
        "{error:#}"
    );
    assert!(started.elapsed() < Duration::from_secs(1), "{error:#}");
}
