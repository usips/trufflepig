use super::*;
use crate::daemon::deadline::QueryDeadline;
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

    assert!(
        crate::coded_error::ErrorCode::TimedOut.is_code_of(&error),
        "{error:#}"
    );
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

    assert!(
        crate::coded_error::ErrorCode::TimedOut.is_code_of(&error),
        "{error:#}"
    );
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

    assert!(
        crate::coded_error::ErrorCode::TimedOut.is_code_of(&error),
        "{error:#}"
    );
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

    assert!(
        crate::coded_error::ErrorCode::TimedOut.is_code_of(&error),
        "{error:#}"
    );
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
    let error = execute_sql(&store.conn, store.deadline.as_ref(), || {
        store.conn.query_row(
            "WITH RECURSIVE counter(value) AS (VALUES(0) UNION ALL SELECT value+1 FROM counter WHERE value<1000000000) SELECT sum(value) FROM counter",
            [],
            |row| row.get::<_, i64>(0),
        )
    })
    .unwrap_err();
    let elapsed = started.elapsed();
    closer.join().unwrap();

    assert!(
        crate::coded_error::ErrorCode::TimedOut.is_code_of(&error),
        "{error:#}"
    );
    assert!(elapsed < Duration::from_secs(1), "elapsed: {elapsed:?}");
}

fn pragma(conn: &Connection, name: &str) -> i64 {
    conn.query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
        .unwrap()
}

fn row_count(conn: &Connection) -> i64 {
    conn.query_row("SELECT count(*) FROM result_sets", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn save_records_payload_bytes_in_a_versioned_incremental_file() {
    let scratch = tempdir().unwrap();
    let store = ResultSetStore::open(scratch.path()).unwrap();

    let id = store.save(set()).unwrap();

    let (bytes, length): (i64, i64) = store
        .conn
        .query_row(
            "SELECT bytes,length(CAST(payload AS BLOB)) FROM result_sets WHERE id=?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(bytes, length);
    assert_eq!(
        pragma(&store.conn, "user_version"),
        result_set_schema::RESULT_SCHEMA_VERSION
    );
    assert_eq!(pragma(&store.conn, "auto_vacuum"), 2, "incremental");
}

#[test]
fn eviction_uses_recorded_bytes_and_keeps_the_new_set() {
    let scratch = tempdir().unwrap();
    let store = ResultSetStore::open(scratch.path()).unwrap();
    let large = store.save(set()).unwrap();
    // Only the recorded size marks this set as filling the cache.
    store
        .conn
        .execute(
            "UPDATE result_sets SET bytes=?1 WHERE id=?2",
            params![MAX_BYTES as i64, large],
        )
        .unwrap();

    let kept = store.save(set()).unwrap();

    let error = store.load(&large).unwrap_err();
    assert!(format!("{error:#}").contains("expired_result"), "{error:#}");
    assert_eq!(store.load(&kept).unwrap().generation, 7);
}

#[test]
fn eviction_removes_oldest_sets_past_the_count_limit() {
    let scratch = tempdir().unwrap();
    let store = ResultSetStore::open(scratch.path()).unwrap();
    let mut ids = Vec::with_capacity(MAX_SETS);
    for created in 0..MAX_SETS {
        let id = store.save(set()).unwrap();
        store
            .conn
            .execute(
                "UPDATE result_sets SET created=?1 WHERE id=?2",
                params![created as i64, id],
            )
            .unwrap();
        ids.push(id);
    }

    let newest = store.save(set()).unwrap();

    assert_eq!(row_count(&store.conn), MAX_SETS as i64);
    assert!(store.load(&ids[0]).is_err());
    assert!(store.load(&ids[1]).is_ok());
    assert!(store.load(&newest).is_ok());
}

#[test]
fn removing_sets_returns_free_pages_to_the_filesystem() {
    let scratch = tempdir().unwrap();
    let store = ResultSetStore::open(scratch.path()).unwrap();
    let payload = "x".repeat(1 << 20);
    for index in 0..3 {
        store
            .conn
            .execute(
                "INSERT INTO result_sets(id,created,expires,bytes,payload) VALUES(?1,0,0,?2,?3)",
                params![format!("expired-{index}"), payload.len() as i64, payload],
            )
            .unwrap();
    }
    let filled = pragma(&store.conn, "page_count");

    store.save(set()).unwrap();

    assert_eq!(row_count(&store.conn), 1);
    assert!(pragma(&store.conn, "page_count") < filled / 2);
    assert!(pragma(&store.conn, "freelist_count") < 16);
}

#[test]
fn stale_layout_is_rebuilt_instead_of_migrated() {
    let scratch = tempdir().unwrap();
    let legacy = Connection::open(scratch.path().join(RESULTS_DATABASE)).unwrap();
    legacy
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE result_sets(id TEXT PRIMARY KEY, created INTEGER NOT NULL, expires INTEGER NOT NULL, payload TEXT NOT NULL);
             INSERT INTO result_sets VALUES('legacy', 0, 9999999999, '{}');",
        )
        .unwrap();
    drop(legacy);
    let deadline = QueryDeadline::after(Duration::from_secs(5));

    let store = ResultSetStore::open_with_deadline(scratch.path(), &deadline).unwrap();

    assert_eq!(
        pragma(&store.conn, "user_version"),
        result_set_schema::RESULT_SCHEMA_VERSION
    );
    assert_eq!(pragma(&store.conn, "auto_vacuum"), 2, "incremental");
    assert_eq!(row_count(&store.conn), 0);
    let id = store.save(set()).unwrap();
    drop(store);
    let reopened = ResultSetStore::open(scratch.path()).unwrap();
    assert_eq!(reopened.load(&id).unwrap().generation, 7);
}
