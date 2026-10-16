use super::*;
use crate::daemon::deadline::{QueryDeadline, is_timed_out};
use std::{os::unix::net::UnixStream, sync::Arc, thread, time::Instant};
use tempfile::tempdir;

fn set() -> StoredResultSet {
    StoredResultSet {
        generation: 7,
        coverage: serde_json::json!({"scope": "home"}),
        truncated: false,
        hits: Vec::new(),
    }
}

#[test]
fn expired_open_does_not_create_cache() {
    let scratch = tempdir().unwrap();
    let cache = scratch.path().join("not-created");
    let deadline = QueryDeadline::after(Duration::ZERO);
    let started = Instant::now();

    let error = match ResultSetStore::open_with_deadline(&cache, &deadline) {
        Ok(_) => panic!("expired deadline unexpectedly opened result cache"),
        Err(error) => error,
    };

    assert!(is_timed_out(&error), "{error:#}");
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(!cache.exists());
}

#[test]
fn deadline_store_saves_and_loads_with_original_snapshot() {
    let scratch = tempdir().unwrap();
    let deadline = QueryDeadline::after(Duration::from_secs(5));
    let store = ResultSetStore::open_with_deadline(scratch.path(), &deadline).unwrap();

    let id = store.save(set()).unwrap();
    let loaded = store.load(&id).unwrap();

    assert_eq!(loaded.generation, 7);
    assert_eq!(loaded.coverage, serde_json::json!({"scope": "home"}));
    assert!(!loaded.truncated);
    assert!(loaded.hits.is_empty());
}

#[test]
fn save_lock_wait_uses_remaining_original_budget_and_rolls_back() {
    let scratch = tempdir().unwrap();
    let _maintenance = ResultSetStore::open(scratch.path()).unwrap();
    let deadline = QueryDeadline::after(Duration::from_millis(800));
    let store = ResultSetStore::open_with_deadline(scratch.path(), &deadline).unwrap();
    let lock = Connection::open(scratch.path().join(RESULTS_DATABASE)).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    thread::sleep(Duration::from_millis(650));

    let started = Instant::now();
    let error = store.save(set()).unwrap_err();
    let elapsed = started.elapsed();
    lock.execute_batch("ROLLBACK").unwrap();

    assert!(is_timed_out(&error), "{error:#}");
    assert!(elapsed < Duration::from_millis(500), "elapsed: {elapsed:?}");
    let count: i64 = lock
        .query_row("SELECT count(*) FROM result_sets", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn load_refuses_after_its_original_deadline_expires() {
    let scratch = tempdir().unwrap();
    let maintenance = ResultSetStore::open(scratch.path()).unwrap();
    let id = maintenance.save(set()).unwrap();
    let deadline = QueryDeadline::after(Duration::from_millis(250));
    let store = ResultSetStore::open_with_deadline(scratch.path(), &deadline).unwrap();
    thread::sleep(Duration::from_millis(300));

    let error = store.load(&id).unwrap_err();

    assert!(is_timed_out(&error), "{error:#}");
}

#[test]
fn peer_drop_cancels_a_locked_save_without_persisting_rows() {
    let scratch = tempdir().unwrap();
    let (server, client) = UnixStream::pair().unwrap();
    let deadline = QueryDeadline::after(Duration::from_secs(10)).with_peer(Arc::new(server));
    let store = ResultSetStore::open_with_deadline(scratch.path(), &deadline).unwrap();
    let lock = Connection::open(scratch.path().join(RESULTS_DATABASE)).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let closer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(80));
        drop(client);
    });

    let started = Instant::now();
    let error = store.save(set()).unwrap_err();
    let elapsed = started.elapsed();
    closer.join().unwrap();
    lock.execute_batch("ROLLBACK").unwrap();

    assert!(is_timed_out(&error), "{error:#}");
    assert!(elapsed < Duration::from_secs(1), "elapsed: {elapsed:?}");
    let count: i64 = lock
        .query_row("SELECT count(*) FROM result_sets", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn peer_drop_interrupts_a_long_sqlite_vm_query() {
    let scratch = tempdir().unwrap();
    let (server, client) = UnixStream::pair().unwrap();
    let deadline = QueryDeadline::after(Duration::from_secs(10)).with_peer(Arc::new(server));
    let store = ResultSetStore::open_with_deadline(scratch.path(), &deadline).unwrap();
    let closer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(80));
        drop(client);
    });

    let started = Instant::now();
    let error = store
        .execute_sql(|| {
            store.conn.query_row(
                "WITH RECURSIVE counter(value) AS (VALUES(0) UNION ALL SELECT value+1 FROM counter WHERE value<1000000000) SELECT sum(value) FROM counter",
                [],
                |row| row.get::<_, i64>(0),
            )
        })
        .unwrap_err();
    let elapsed = started.elapsed();
    closer.join().unwrap();

    assert!(is_timed_out(&error), "{error:#}");
    assert!(elapsed < Duration::from_secs(1), "elapsed: {elapsed:?}");
}
