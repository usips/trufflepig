use super::*;
use crate::store::{Store, is_index_warming};
use std::time::Duration;

fn indexed_fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("root");
    let cache = directory.path().join("cache");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("lib.rs"), "pub fn alpha() {}\n").unwrap();
    Store::open(&root, &cache).unwrap().index().unwrap();
    (directory, root, cache)
}

fn raw(cache: &Path) -> Connection {
    Connection::open(cache.join(INDEX_NAME)).unwrap()
}

fn pragma(conn: &Connection, name: &str) -> i64 {
    conn.pragma_query_value(None, name, |row| row.get(0))
        .unwrap()
}

#[test]
fn new_index_is_stamped_and_incrementally_vacuumed() {
    let (_directory, _root, cache) = indexed_fixture();
    let conn = raw(&cache);
    assert_eq!(pragma(&conn, "user_version"), schema::INDEX_SCHEMA_REVISION);
    assert_eq!(pragma(&conn, "auto_vacuum"), AUTO_VACUUM_INCREMENTAL);
    assert_eq!(pragma(&conn, "freelist_count"), 0);
    assert_eq!(
        std::fs::metadata(cache.join(format!("{INDEX_NAME}-wal")))
            .map_or(0, |metadata| metadata.len()),
        0,
        "publication truncates the WAL when no reader holds it"
    );
}

#[test]
fn mismatched_schema_revision_reads_as_warming_then_rebuilds() {
    let (_directory, root, cache) = indexed_fixture();
    {
        let conn = raw(&cache);
        conn.execute_batch("CREATE TABLE result_sets(id INTEGER PRIMARY KEY);")
            .unwrap();
        conn.pragma_update(None, "user_version", schema::INDEX_SCHEMA_REVISION + 1)
            .unwrap();
    }
    let read = Store::open_read(&root, &cache, QueryDeadline::after(Duration::from_secs(5)));
    assert!(is_index_warming(&read.err().unwrap()));

    let mut store = Store::open(&root, &cache).unwrap();
    assert_eq!(store.generation().unwrap(), 0);
    assert_eq!(
        pragma(&store.conn, "user_version"),
        schema::INDEX_SCHEMA_REVISION
    );
    assert_eq!(pragma(&store.conn, "auto_vacuum"), AUTO_VACUUM_INCREMENTAL);
    let stale: i64 = store
        .conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='result_sets'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stale, 0);
    store.index().unwrap();
    assert_eq!(store.generation().unwrap(), 1);
    Store::open_read(&root, &cache, QueryDeadline::after(Duration::from_secs(5))).unwrap();
}

#[test]
fn mismatched_schema_revision_waits_for_a_free_writer_lease() {
    let (_directory, root, cache) = indexed_fixture();
    raw(&cache)
        .pragma_update(None, "user_version", schema::INDEX_SCHEMA_REVISION + 1)
        .unwrap();
    let lease = std::fs::File::open(cache.join("index.lock")).unwrap();
    fs2::FileExt::lock_exclusive(&lease).unwrap();

    let error = Store::open(&root, &cache).err().unwrap();
    assert!(is_index_warming(&error), "{error:#}");
    let generation: String = raw(&cache)
        .query_row("SELECT value FROM meta WHERE key='generation'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(generation, "1");
}

#[test]
fn unstamped_legacy_index_is_discarded() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("root");
    let cache = directory.path().join("cache");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    raw(&cache)
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
             INSERT INTO meta VALUES('generation','7');
             CREATE TABLE files(path TEXT, id INTEGER PRIMARY KEY);
             CREATE VIRTUAL TABLE documents USING fts5(body);",
        )
        .unwrap();
    let store = Store::open(&root, &cache).unwrap();
    assert_eq!(store.generation().unwrap(), 0);
    let columns: i64 = store
        .conn
        .query_row(
            "SELECT count(*) FROM pragma_table_info('files')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(columns, 6);
}

#[test]
fn corrupt_index_is_quarantined_and_rebuilt() {
    let (_directory, root, cache) = indexed_fixture();
    std::fs::write(cache.join(INDEX_NAME), vec![0x5a; 8192]).unwrap();
    let _ = std::fs::remove_file(cache.join(format!("{INDEX_NAME}-wal")));
    let _ = std::fs::remove_file(cache.join(format!("{INDEX_NAME}-shm")));

    let mut store = Store::open(&root, &cache).unwrap();
    assert_eq!(store.generation().unwrap(), 0);
    assert_eq!(
        std::fs::read(cache.join(QUARANTINE_NAME)).unwrap(),
        vec![0x5a; 8192]
    );
    store.index().unwrap();
    assert_eq!(store.generation().unwrap(), 1);
}

#[test]
fn corrupt_index_is_left_alone_while_another_writer_holds_the_lease() {
    let (_directory, root, cache) = indexed_fixture();
    std::fs::write(cache.join(INDEX_NAME), vec![0x5a; 8192]).unwrap();
    let _ = std::fs::remove_file(cache.join(format!("{INDEX_NAME}-wal")));
    let _ = std::fs::remove_file(cache.join(format!("{INDEX_NAME}-shm")));
    let lease = std::fs::File::open(cache.join("index.lock")).unwrap();
    fs2::FileExt::lock_exclusive(&lease).unwrap();

    let error = Store::open(&root, &cache).err().unwrap();
    assert!(is_index_corrupt(&error), "{error:#}");
    assert!(!cache.join(QUARANTINE_NAME).exists());
}

#[test]
fn corruption_found_by_an_unbounded_pass_is_quarantined_for_the_next_pass() {
    let (_directory, root, cache) = indexed_fixture();
    let (page_size, root_page): (i64, i64) = {
        let conn = raw(&cache);
        (
            pragma(&conn, "page_size"),
            conn.query_row(
                "SELECT rootpage FROM sqlite_master WHERE name='extraction_cache'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
        )
    };
    let mut store = Store::open(&root, &cache).unwrap();
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(cache.join(INDEX_NAME))
            .unwrap();
        file.seek(SeekFrom::Start(((root_page - 1) * page_size) as u64))
            .unwrap();
        file.write_all(&vec![0xff; page_size as usize]).unwrap();
    }

    let error = store.index().unwrap_err();
    assert!(is_index_corrupt(&error), "{error:#}");
    assert!(cache.join(QUARANTINE_NAME).exists());
    assert_eq!(store.generation().unwrap(), 0);
    store.index().unwrap();
    assert_eq!(store.generation().unwrap(), 1);
}
