use super::*;
use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[test]
fn workspace_result_save_busy_wait_uses_the_open_deadline() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cache = directory.path();
    let setup_deadline = QueryDeadline::start();
    WorkspaceResults::create(cache, &setup_deadline)?;

    let deadline = QueryDeadline::after(Duration::from_millis(800));
    let results = WorkspaceResults::open(cache, &deadline)?;
    let mut writer = Connection::open(cache.join("workspace.sqlite3"))?;
    let _lock = writer.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    thread::sleep(Duration::from_millis(650));
    let remaining = deadline.remaining();
    assert!(remaining <= Duration::from_millis(200));
    let started = Instant::now();
    let error = results
        .save(WorkspaceSet {
            workspace: "test".into(),
            home: None,
            owners: vec![],
            coverage: vec![],
            hits: vec![],
            truncated: false,
            scope: None,
        })
        .unwrap_err();

    assert!(
        tp_core::coded_error::ErrorCode::TimedOut.is_code_of(&error),
        "{error:#}"
    );
    assert!(started.elapsed() < Duration::from_millis(500));
    drop(_lock);
    let verify = Connection::open(cache.join("workspace.sqlite3"))?;
    let count: i64 = verify.query_row("SELECT count(*) FROM workspace_results", [], |row| {
        row.get(0)
    })?;
    assert_eq!(count, 0);
    Ok(())
}

#[test]
fn workspace_result_save_retries_busy_until_writer_releases() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cache = directory.path();
    let setup_deadline = QueryDeadline::start();
    WorkspaceResults::create(cache, &setup_deadline)?;

    let deadline = QueryDeadline::after(Duration::from_secs(2));
    let results = WorkspaceResults::open(cache, &deadline)?;
    let database = cache.join("workspace.sqlite3");
    let (locked_tx, locked_rx) = mpsc::sync_channel(0);
    let writer_thread = thread::spawn(move || -> Result<()> {
        let mut writer = Connection::open(database)?;
        let lock = writer.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        locked_tx.send(()).expect("test receiver remains open");
        thread::sleep(Duration::from_millis(180));
        drop(lock);
        Ok(())
    });
    locked_rx.recv().expect("writer acquired its lock");

    let id = results.save(WorkspaceSet {
        workspace: "test".into(),
        home: None,
        owners: vec![],
        coverage: vec![],
        hits: vec![],
        truncated: false,
        scope: None,
    })?;
    writer_thread.join().expect("writer thread did not panic")?;

    let count: i64 = results.conn.query_row(
        "SELECT count(*) FROM workspace_results WHERE id=?1",
        [&id],
        |row| row.get(0),
    )?;
    assert_eq!(count, 1);
    Ok(())
}

#[test]
fn workspace_result_open_rejects_an_expired_deadline() {
    let directory = tempfile::tempdir().unwrap();
    let deadline = QueryDeadline::after(Duration::ZERO);
    let error = match WorkspaceResults::open(directory.path(), &deadline) {
        Err(error) => error,
        Ok(_) => panic!("expired deadline opened workspace results"),
    };
    assert!(
        tp_core::coded_error::ErrorCode::TimedOut.is_code_of(&error),
        "{error:#}"
    );
}
