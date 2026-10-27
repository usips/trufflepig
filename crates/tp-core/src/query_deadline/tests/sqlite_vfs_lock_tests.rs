use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

const CHILD_ENV: &str = "TRUFFLEPIG_SQLITE_VFS_LOCK_CHILD";
const DIRECTORY_ENV: &str = "TRUFFLEPIG_SQLITE_VFS_LOCK_DIRECTORY";
const CHILD_TEST: &str =
    "query_deadline::tests::sqlite_vfs_lock_tests::wal_close_and_cache_write_progress_is_bounded";
const CONNECTION_CYCLES: usize = 64;
const CHILD_TIMEOUT: Duration = Duration::from_secs(10);
const COMPLETION_FILE: &str = "wal-progress-complete";

#[test]
fn bundled_sqlite_includes_unix_vfs_and_wal_reset_fixes() {
    // https://sqlite.org/releaselog/3_51_2.html and /3_51_3.html
    assert!(
        rusqlite::version_number() >= 3_051_003,
        "bundled SQLite {} is older than 3.51.3; 3.51.2 fixes the Unix VFS deadlock and 3.51.3 fixes the WAL-reset bug",
        rusqlite::version()
    );
}

#[test]
fn wal_close_and_cache_write_progress_is_bounded() {
    if std::env::var_os(CHILD_ENV).is_some() {
        run_wal_progress_child().unwrap();
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .arg(CHILD_TEST)
        .arg("--exact")
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .env(DIRECTORY_ENV, directory.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + CHILD_TIMEOUT;

    loop {
        if child.try_wait().unwrap().is_some() {
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "SQLite WAL child failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                directory.path().join(COMPLETION_FILE).is_file(),
                "SQLite WAL child must execute the progress assertions"
            );
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!(
                "SQLite WAL child exceeded {CHILD_TIMEOUT:?}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn run_wal_progress_child() -> Result<(), Box<dyn Error>> {
    let directory = std::env::var_os(DIRECTORY_ENV)
        .map(PathBuf::from)
        .ok_or("missing SQLite WAL child directory")?;
    let query_database = directory.join("query.sqlite3");
    let cache_database = directory.join("result-cache.sqlite3");
    initialize_wal_database(&query_database)?;
    initialize_wal_database(&cache_database)?;

    let start = Arc::new(Barrier::new(4));
    let paired_readers = Arc::new(Barrier::new(2));
    let query_progress = Arc::new(AtomicUsize::new(0));
    let cache_progress = Arc::new(AtomicUsize::new(0));

    let query_workers = (0..2)
        .map(|_| {
            let start = Arc::clone(&start);
            let paired_readers = Arc::clone(&paired_readers);
            let query_progress = Arc::clone(&query_progress);
            let query_database = query_database.clone();
            thread::spawn(move || -> rusqlite::Result<()> {
                start.wait();
                for _ in 0..CONNECTION_CYCLES {
                    let mut connection = Connection::open_with_flags(
                        &query_database,
                        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                    )?;
                    let transaction = connection.transaction()?;
                    let value: i64 = transaction.query_row(
                        "SELECT value FROM progress WHERE id = 1",
                        [],
                        |row| row.get(0),
                    )?;
                    assert_eq!(value, 0);
                    paired_readers.wait();
                    drop(transaction);
                    drop(connection);
                    query_progress.fetch_add(1, Ordering::Relaxed);
                }
                Ok(())
            })
        })
        .collect::<Vec<_>>();

    let cache_progress_for_writer = Arc::clone(&cache_progress);
    let writer_start = Arc::clone(&start);
    let cache_writer = thread::spawn(move || -> rusqlite::Result<()> {
        writer_start.wait();
        for _ in 0..CONNECTION_CYCLES {
            let mut connection = Connection::open(&cache_database)?;
            connection.busy_timeout(Duration::from_secs(1))?;
            connection.pragma_update(None, "wal_autocheckpoint", 1)?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute("UPDATE progress SET value = value + 1 WHERE id = 1", [])?;
            transaction.commit()?;
            drop(connection);
            cache_progress_for_writer.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    });

    start.wait();
    for worker in query_workers {
        worker.join().expect("query worker panicked")?;
    }
    cache_writer.join().expect("cache writer panicked")?;

    assert_eq!(
        query_progress.load(Ordering::Relaxed),
        2 * CONNECTION_CYCLES,
        "both readers must finish their WAL close cycles"
    );
    assert_eq!(
        cache_progress.load(Ordering::Relaxed),
        CONNECTION_CYCLES,
        "the result-cache WAL writer must continue to make progress"
    );
    std::fs::write(directory.join(COMPLETION_FILE), b"complete")?;
    Ok(())
}

fn initialize_wal_database(path: &Path) -> rusqlite::Result<()> {
    let connection = Connection::open(path)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.execute_batch(
        "CREATE TABLE progress (id INTEGER PRIMARY KEY, value INTEGER NOT NULL);
         INSERT INTO progress (id, value) VALUES (1, 0);",
    )?;
    Ok(())
}
