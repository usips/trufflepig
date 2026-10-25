use super::*;
use crate::board::board_runtime_markers::validate_board_database;

#[test]
fn losing_router_start_cannot_replace_the_live_database_marker() {
    let directory = tp_core::test_fixtures::scratch("board-runtime-");
    let runtime = directory.path().join("runtime");
    let spool = directory.path().join("spool");
    let live_database = directory.path().join("live.sqlite3");
    let loser_database = directory.path().join("loser.sqlite3");
    let router = |database: &Path| SystemRouter {
        runtime: Some(runtime.clone()),
        cache_base: None,
        sweeps: Mutex::new(SweepClock::default()),
        board: crate::board::BoardHost::with_config(crate::board::BoardConfig::for_database(
            database,
        )),
    };
    let live = router(&live_database);
    let live_runtime = runtime.clone();
    let live_spool = spool.clone();
    let worker = std::thread::spawn(move || daemon::serve_router(&live_runtime, &live_spool, live));
    let deadline = Instant::now() + Duration::from_secs(3);
    let ping = ["system".into(), "status".into()];
    let context = RequestContext::new(None, None);
    loop {
        if tp_core::daemon_transport::request(&runtime, &ping, &context)
            .unwrap()
            .is_some()
        {
            break;
        }
        assert!(Instant::now() < deadline, "live router failed to start");
        std::thread::sleep(Duration::from_millis(10));
    }
    let rejected = daemon::serve_router(&runtime, &spool, router(&loser_database));
    assert!(rejected.is_err());
    let accepted = validate_board_database(&runtime, &live_database);
    let refused = validate_board_database(&runtime, &loser_database);
    tp_core::daemon_transport::stop(&runtime).unwrap();
    worker.join().unwrap().unwrap();
    accepted.unwrap();
    assert!(refused.is_err());
    assert!(!loser_database.exists());
}
