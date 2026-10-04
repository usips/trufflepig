use super::*;
use crate::board::board_protocol::ClaimResume;

#[test]
fn maintenance_import_backoff_caps_and_recovers_without_hot_polling() {
    let now = Instant::now();
    let mut clock = BoardMaintenanceClock::default();
    assert!(clock.check_due(now));
    assert!(!clock.check_due(now + Duration::from_millis(1)));
    assert!(clock.check_due(now + Duration::from_secs(2)));
    let pending = crate::board::feedback_outbox::ImportSummary {
        pending: 1,
        ..Default::default()
    };
    clock.import_finished(now, &pending);
    assert!(!clock.import_ready(now + Duration::from_secs(1)));
    assert!(clock.import_ready(now + Duration::from_secs(2)));
    for _ in 0..10 {
        clock.import_finished(now, &pending);
    }
    assert!(!clock.import_ready(now + Duration::from_secs(59)));
    assert!(clock.import_ready(now + Duration::from_secs(60)));
    clock.import_finished(now, &Default::default());
    assert!(clock.import_ready(now));
    clock.import_finished(now, &pending);
    assert!(clock.import_ready(now + Duration::from_secs(2)));
}

#[test]
fn configuration_ttl_refresh_changes_existing_writer_claim_policy() {
    let directory = crate::board::board_test_support::scratch("board-runtime-");
    let database = directory.path().join("board.sqlite3");
    let mut config = BoardConfig::for_database(&database);
    let host = BoardHost::with_config(config.clone());
    let actor = config.actor(None, Some("claim-owner")).unwrap();
    let create = BoardRequest::new(
        actor.clone(),
        BoardOp::New {
            title: crate::board::board_vocabulary::PlanTitle::new("TTL test").unwrap(),
            body: crate::board::board_vocabulary::PlanText::new("").unwrap(),
            steward: None,
            repo_key: None,
        },
    );
    let reply = host.handle_by(&create, QueryDeadline::start()).unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("expected plan");
    };
    let plan = change.plan.unwrap();
    let reply = host
        .handle_by(
            &BoardRequest::new(
                actor.clone(),
                BoardOp::CarveClaim {
                    plan,
                    title: crate::board::board_vocabulary::PlanTitle::new("Owned task").unwrap(),
                    scope: crate::board::board_vocabulary::EntryText::new("ttl policy").unwrap(),
                    section: None,
                },
            ),
            QueryDeadline::start(),
        )
        .unwrap();
    let BoardResult::Change(change) = reply.result else {
        panic!("expected task");
    };
    let task = change.task.unwrap();
    let external = rusqlite::Connection::open(&database).unwrap();
    external
        .execute("UPDATE claims SET last_active=last_active-120", [])
        .unwrap();
    config.claim_ttl_minutes = 1;
    *host.inner.config.lock().unwrap() = BoardConfigCache::with_config(config.clone());
    let takeover = config.actor(None, Some("claim-takeover")).unwrap();
    let request = BoardRequest::new(
        takeover,
        BoardOp::ClaimTask {
            task,
            scope: Some(crate::board::board_vocabulary::EntryText::new("new owner").unwrap()),
            resume: ClaimResume::No,
        },
    );
    host.handle_by(&request, QueryDeadline::start()).unwrap();
}

#[test]
fn idle_maintenance_schedules_a_blocked_loader_without_waiting() {
    use std::os::unix::ffi::OsStrExt;
    let directory = crate::board::board_test_support::scratch("board-runtime-");
    let source = directory.path().join("blocked-board.toml");
    let name = std::ffi::CString::new(source.as_os_str().as_bytes()).unwrap();
    // SAFETY: CString is NUL terminated and mkfifo only creates this test path.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let host = BoardHost::default();
    *host.inner.config.lock().unwrap() = BoardConfigCache::with_source(
        source.clone(),
        BoardConfig::for_database(directory.path().join("absent.sqlite3")),
    );
    let started = Instant::now();
    host.idle();
    assert!(started.elapsed() < Duration::from_millis(100));
    let worker = host
        .inner
        .maintenance
        .lock()
        .unwrap()
        .running
        .take()
        .unwrap();
    std::fs::write(source, "claim_ttl_minutes = 1").unwrap();
    worker.join().unwrap().unwrap();
}

#[test]
fn idle_maintenance_does_not_wait_for_configuration_lock() {
    let directory = crate::board::board_test_support::scratch("board-runtime-");
    let host = BoardHost::with_config(BoardConfig::for_database(
        directory.path().join("absent-board.sqlite3"),
    ));
    let held = host.inner.config.lock().unwrap();
    let started = Instant::now();
    host.idle();
    assert!(started.elapsed() < Duration::from_millis(100));
    drop(held);
}

#[test]
fn backend_rejects_client_only_web_before_opening_board_database() {
    let directory = crate::board::board_test_support::scratch("board-web-client-only-");
    let database = directory.path().join("absent.sqlite3");
    let host = BoardHost::with_config(BoardConfig::for_database(&database));
    let options = crate::cli::parse(&["board".into(), "web".into(), "P1".into()]).unwrap();
    let error = host
        .run(
            &options,
            &RequestContext::new(None, None),
            QueryDeadline::start(),
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("invalid_options: board web must run on the client")
    );
    assert!(!database.exists());
    assert!(host.inner.backend.lock().unwrap().is_none());
}
