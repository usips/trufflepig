use super::*;

use crate::query_deadline::QueryDeadline;
use fs2::FileExt;
use rusqlite::Connection;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, crate::store::Store) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("root");
    let cache = directory.path().join("cache");
    fs::create_dir(&root).unwrap();
    let store = crate::store::Store::open(&root, &cache).unwrap();
    (directory, root, cache, store)
}

fn retained_file_count(cache: &Path) -> usize {
    let path = cache.join(CHECKPOINT_DIRECTORY).join(CHECKPOINT_DATABASE);
    if !path.exists() {
        return 0;
    }
    let connection = Connection::open(path).unwrap();
    connection
        .query_row("SELECT count(*) FROM completed_files", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap() as usize
}

fn lease(cache: &Path) -> std::fs::File {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join("index.lock"))
        .unwrap()
}

fn retain_fixture_sources(
    store: &crate::store::Store,
    root: &Path,
    cache: &Path,
) -> IndexCheckpoint {
    let budget = QueryDeadline::start();
    let mut checkpoint = IndexCheckpoint::open(&store.conn, root, cache, Some(&budget)).unwrap();
    checkpoint.begin_pass().unwrap();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        let bytes = fs::read(&path).unwrap();
        let relative = path.strip_prefix(root).unwrap();
        let path = crate::path_codec::encode_path(relative);
        let revision = blake3::hash(&bytes).to_hex().to_string();
        let extraction = tp_extract::extract(&path, &bytes);
        checkpoint
            .completed_file(
                &path,
                Some(&revision),
                "read",
                bytes.len() as u64,
                crate::store::scan::extraction_cache_version(),
                &extraction,
            )
            .unwrap();
    }
    checkpoint.pass_completed().unwrap();
    checkpoint
}

fn coverage_observation(coverage: &crate::store::Coverage) -> (usize, usize, usize, u64) {
    (
        coverage.total_files,
        coverage.indexed_files,
        coverage.walk_failures,
        coverage.indexed_bytes,
    )
}

#[test]
fn index_until_polls_released_lease_but_available_attempt_returns_promptly() {
    let (_directory, root, cache, mut store) = fixture();
    fs::write(root.join("one.rs"), "pub fn retained() {}\n").unwrap();
    store.index().unwrap();
    let original = store.publication().unwrap().unwrap();
    fs::write(root.join("one.rs"), "pub fn changed() {}\n").unwrap();

    let held = lease(&cache);
    held.lock_exclusive().unwrap();
    let started = Instant::now();
    let skipped = store
        .index_available_until(QueryDeadline::after(Duration::from_secs(1)))
        .unwrap();
    assert!(skipped.is_none());
    assert!(started.elapsed() < Duration::from_millis(100));
    assert_eq!(store.publication().unwrap().unwrap(), original);
    drop(held);

    let held = lease(&cache);
    held.lock_exclusive().unwrap();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(75));
        drop(held);
    });
    let started = Instant::now();
    let indexed = store
        .index_until(QueryDeadline::after(Duration::from_secs(2)))
        .unwrap();
    assert!(indexed.is_some());
    assert!(started.elapsed() >= Duration::from_millis(50));
    assert_eq!(store.generation().unwrap(), original.generation + 1);
}

#[test]
fn bounded_slices_with_a_save_budget_reuse_retained_extractions_until_publish() {
    let (_directory, root, cache, _store) = fixture();
    const FILES: usize = 1024;
    const SAVES_PER_SLICE: usize = 100;
    for number in 0..FILES {
        fs::write(
            root.join(format!("file-{number:04}.txt")),
            format!("A small retained text observation, file {number}.\n"),
        )
        .unwrap();
    }

    let slices = FILES.div_ceil(SAVES_PER_SLICE);
    TEST_SAVE_BUDGET.set(Some(SAVES_PER_SLICE));
    for slice in 1..=slices {
        let mut store = crate::store::Store::open(&root, &cache).unwrap();
        let result = store.index_until(QueryDeadline::start());
        let progress = crate::store::read_index_progress(&root, &cache);
        if slice < slices {
            let error = result
                .err()
                .expect("a slice stops once its save budget is spent");
            assert!(
                crate::coded_error::ErrorCode::TimedOut.is_code_of(&error),
                "{error:#}"
            );
            assert_eq!(retained_file_count(&cache), slice * SAVES_PER_SLICE);
            let progress = progress.expect("an interrupted slice persists progress");
            assert_eq!(progress.reused_files, (slice - 1) * SAVES_PER_SLICE);
            assert!(!progress.maintenance_required);
            assert_eq!(store.generation().unwrap(), 0);
        } else {
            let coverage = result.unwrap().expect("the final slice publishes");
            assert_eq!(coverage.total_files, FILES);
            assert_eq!(store.generation().unwrap(), 1);
        }
    }
    TEST_SAVE_BUDGET.set(None);
    assert!(crate::store::read_index_progress(&root, &cache).is_none());
    assert!(!cache.join(CHECKPOINT_DIRECTORY).exists());
}

#[test]
fn one_stalled_slice_does_not_require_maintenance_when_the_next_slice_progresses() {
    let (_directory, root, cache, store) = fixture();
    fs::write(root.join("sample.rs"), "pub fn progressing() {}\n").unwrap();
    let source = fs::read(root.join("sample.rs")).unwrap();
    let path = crate::path_codec::encode_path(Path::new("sample.rs"));
    let revision = blake3::hash(&source).to_hex().to_string();
    let extraction = tp_extract::extract(&path, &source);
    let budget = QueryDeadline::start();

    let mut stalled = IndexCheckpoint::open(&store.conn, &root, &cache, Some(&budget)).unwrap();
    stalled.begin_pass().unwrap();
    stalled.note_stalled_slice().unwrap();
    assert!(!stalled.maintenance_required());
    drop(stalled);

    let mut progressing = IndexCheckpoint::open(&store.conn, &root, &cache, Some(&budget)).unwrap();
    progressing.begin_pass().unwrap();
    progressing
        .completed_file(
            &path,
            Some(&revision),
            "read",
            source.len() as u64,
            crate::store::scan::extraction_cache_version(),
            &extraction,
        )
        .unwrap();
    progressing.note_stalled_slice().unwrap();
    assert!(!progressing.maintenance_required());
    assert_eq!(progressing.stalled_slices, 0);
    for stalled in 1..=3 {
        progressing.begin_pass().unwrap();
        progressing.note_stalled_slice().unwrap();
        assert_eq!(progressing.maintenance_required(), stalled == 3);
    }
}

#[test]
fn unbounded_reconcile_reads_retained_facts_and_commits_once_per_pass() {
    let (_directory, root, cache, store) = fixture();
    const FILES: usize = 300;
    for number in 0..FILES {
        fs::write(
            root.join(format!("file-{number:03}.txt")),
            format!("Retained observation {number}.\n"),
        )
        .unwrap();
    }
    drop(retain_fixture_sources(&store, &root, &cache));
    assert_eq!(retained_file_count(&cache), FILES);

    let mut checkpoint = IndexCheckpoint::open(&store.conn, &root, &cache, None).unwrap();
    assert!(checkpoint.has_retained_facts());
    assert!(!checkpoint.retains_new_extractions());
    checkpoint.begin_pass().unwrap();
    for number in 0..FILES {
        checkpoint
            .reused_file(&format!("file-{number:03}.txt"))
            .unwrap();
    }
    checkpoint.pass_completed().unwrap();
    assert_eq!(checkpoint.commit_count(), 1);
    assert_eq!(retained_file_count(&cache), FILES);
}

#[test]
fn bounded_read_of_unchanged_published_repo_writes_no_checkpoint() {
    let (_directory, root, cache, mut store) = fixture();
    const FILES: usize = 2_000;
    for number in 0..FILES {
        fs::write(
            root.join(format!("item_{number:04}.rs")),
            format!("pub fn item_{number}() {{}}\n"),
        )
        .unwrap();
    }
    store.index().unwrap();

    let started = Instant::now();
    let coverage = store
        .index_until(QueryDeadline::start())
        .unwrap()
        .expect("an unchanged published repository reconciles within the request budget");
    let elapsed = started.elapsed();
    assert_eq!(coverage.total_files, FILES);
    assert_eq!(store.generation().unwrap(), 1);
    assert!(!cache.join(CHECKPOINT_DIRECTORY).exists());
    // Smoke bound only: the baseline's per-file fsyncs exceeded the 20 s budget here.
    assert!(
        elapsed < Duration::from_secs(5),
        "unchanged reconcile took {elapsed:?}"
    );
}

#[test]
fn resumed_scan_invalidates_changed_and_deleted_completed_sources() {
    let (_directory, root, cache, mut store) = fixture();
    fs::write(
        root.join("a_changed.rs"),
        "pub fn stale_changed_name() {}\n",
    )
    .unwrap();
    fs::write(
        root.join("b_deleted.rs"),
        "pub fn stale_deleted_name() {}\n",
    )
    .unwrap();
    let initial_coverage = store.index().unwrap();
    let initial_publication = store.publication().unwrap().unwrap();
    let original_generation = store.generation().unwrap();
    drop(retain_fixture_sources(&store, &root, &cache));
    assert_eq!(retained_file_count(&cache), 2);
    for number in 0..1_024 {
        fs::write(
            root.join(format!("later-{number:04}.txt")),
            format!("A later text observation, file {number}.\n"),
        )
        .unwrap();
    }

    TEST_SAVE_BUDGET.set(Some(1));
    let bounded_pass = store.index_available_until(QueryDeadline::start());
    TEST_SAVE_BUDGET.set(None);
    match bounded_pass {
        Err(error) => assert!(
            crate::coded_error::ErrorCode::TimedOut.is_code_of(&error),
            "{error:#}"
        ),
        Ok(None) => {}
        Ok(Some(_)) => panic!("fixture must stop after its first new saved extraction"),
    }
    assert!(retained_file_count(&cache) >= 2);
    assert_eq!(store.generation().unwrap(), original_generation);
    assert_eq!(store.publication().unwrap().unwrap(), initial_publication);
    assert_eq!(
        coverage_observation(&store.coverage().unwrap()),
        coverage_observation(&initial_coverage)
    );
    assert!(crate::store::read_index_progress(&root, &cache).is_some());

    fs::write(
        root.join("a_changed.rs"),
        "pub fn fresh_changed_name() {}\n",
    )
    .unwrap();
    fs::remove_file(root.join("b_deleted.rs")).unwrap();
    assert_eq!(store.publication().unwrap().unwrap(), initial_publication);
    drop(store);
    let mut store = crate::store::Store::open(&root, &cache).unwrap();
    let coverage = store.index().unwrap();
    assert_eq!(coverage.total_files, 1_025);
    assert_eq!(store.generation().unwrap(), original_generation + 1);
    let fresh: i64 = store
        .conn
        .query_row(
            "SELECT count(*) FROM definitions WHERE name='fresh_changed_name'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let stale_changed: i64 = store
        .conn
        .query_row(
            "SELECT count(*) FROM definitions WHERE name='stale_changed_name'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let stale_deleted: i64 = store
        .conn
        .query_row(
            "SELECT count(*) FROM definitions WHERE name='stale_deleted_name'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!((fresh, stale_changed, stale_deleted), (1, 0, 0));
    assert!(crate::store::read_index_progress(&root, &cache).is_none());
}

#[test]
fn expired_resolver_records_maintenance_and_explicit_index_finishes_retained_work() {
    let (_directory, root, cache, mut store) = fixture();
    const FILES: usize = 96;
    for number in 0..FILES {
        fs::write(
            root.join(format!("file-{number:03}.txt")),
            format!("Retained observation {number}.\n"),
        )
        .unwrap();
    }
    let checkpoint = retain_fixture_sources(&store, &root, &cache);
    drop(checkpoint);
    // Each attempt reuses every retained file and saves nothing new, so it stalls.
    for stalled in 1..=3 {
        let writer_lock = lease(&cache);
        writer_lock.lock_exclusive().unwrap();
        let deadline = QueryDeadline::after(Duration::from_secs(2));
        let overrun = deadline.remaining() + Duration::from_millis(25);
        let result = store.index_leased(
            writer_lock,
            |_| {
                std::thread::sleep(overrun);
                Ok(())
            },
            Some(&deadline),
        );
        let error = result.unwrap_err();
        assert!(
            crate::coded_error::ErrorCode::TimedOut.is_code_of(&error),
            "{error:#}"
        );
        assert_eq!(retained_file_count(&cache), FILES);
        let progress = crate::store::read_index_progress(&root, &cache).unwrap();
        assert_eq!(progress.maintenance_required, stalled == 3);
        assert_eq!(progress.completed_files, FILES);
    }
    let progress = crate::store::read_index_progress(&root, &cache).unwrap();
    assert_eq!(progress.phase, IndexProgressPhase::MaintenanceRequired);

    let started = Instant::now();
    assert!(
        store
            .index_available_until(QueryDeadline::after(Duration::from_secs(1)))
            .unwrap()
            .is_none()
    );
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(store.generation().unwrap(), 0);

    let coverage = store.index().unwrap();
    assert_eq!(coverage.total_files, FILES);
    assert_eq!(store.generation().unwrap(), 1);
    assert!(crate::store::read_index_progress(&root, &cache).is_none());
}

#[test]
fn caller_cancellation_keeps_resumable_progress_without_maintenance_poison() {
    let (directory, root, cache, mut store) = fixture();
    fs::write(root.join("sample.rs"), "pub fn recoverable() {}\n").unwrap();
    let claim = directory.path().join("claim");
    fs::write(&claim, "active").unwrap();
    let deadline = QueryDeadline::after(Duration::from_secs(3)).with_claim_path(claim.clone());
    let writer_lock = lease(&cache);
    writer_lock.lock_exclusive().unwrap();
    let result = store.index_leased(
        writer_lock,
        |_| {
            fs::remove_file(&claim)?;
            Ok(())
        },
        Some(&deadline),
    );
    let error = result.unwrap_err();
    assert!(
        crate::coded_error::ErrorCode::TimedOut.is_code_of(&error),
        "{error:#}"
    );
    let progress = crate::store::read_index_progress(&root, &cache).unwrap();
    assert!(!progress.maintenance_required);

    let coverage = store
        .index_available_until(QueryDeadline::after(Duration::from_secs(1)))
        .unwrap()
        .expect("a caller cancellation does not suppress the next bounded attempt");
    assert_eq!(coverage.total_files, 1);
    assert_eq!(store.generation().unwrap(), 1);
    assert!(crate::store::read_index_progress(&root, &cache).is_none());
}

#[test]
fn corrupt_retained_contract_is_hidden_and_invalidated_before_reuse() {
    for (key, value) in [
        ("root", "corrupt-root"),
        ("contract", "corrupt-contract"),
        ("base_epoch", "corrupt-epoch"),
        ("base_generation", "999"),
    ] {
        let (_directory, root, cache, store) = fixture();
        let budget = QueryDeadline::start();
        let mut checkpoint =
            IndexCheckpoint::open(&store.conn, &root, &cache, Some(&budget)).unwrap();
        checkpoint.begin_pass().unwrap();
        checkpoint
            .completed_file(
                "sample.rs",
                Some("revision"),
                "read",
                1,
                crate::store::scan::extraction_cache_version(),
                &Extraction::default(),
            )
            .unwrap();
        checkpoint.pass_completed().unwrap();
        drop(checkpoint);
        assert!(crate::store::read_index_progress(&root, &cache).is_some());

        let stage = cache.join(CHECKPOINT_DIRECTORY).join(CHECKPOINT_DATABASE);
        Connection::open(&stage)
            .unwrap()
            .execute(
                "UPDATE checkpoint_meta SET value=?1 WHERE key=?2",
                rusqlite::params![value, key],
            )
            .unwrap();
        assert!(crate::store::read_index_progress(&root, &cache).is_none());

        let checkpoint = IndexCheckpoint::open(&store.conn, &root, &cache, None).unwrap();
        assert!(!checkpoint.has_retained_facts(), "mismatch for {key}");
        assert_eq!(retained_file_count(&cache), 0, "mismatch for {key}");
    }
}

#[test]
fn corrupt_retained_database_is_discarded_and_explicit_index_recovers() {
    let (_directory, root, cache, mut store) = fixture();
    fs::write(root.join("sample.rs"), "pub fn recovered() {}\n").unwrap();
    let checkpoint = retain_fixture_sources(&store, &root, &cache);
    drop(checkpoint);
    fs::write(
        cache.join(CHECKPOINT_DIRECTORY).join(CHECKPOINT_DATABASE),
        b"not a sqlite database",
    )
    .unwrap();

    assert!(crate::store::read_index_progress(&root, &cache).is_none());
    let coverage = store.index().unwrap();
    assert_eq!(coverage.total_files, 1);
    let recovered: i64 = store
        .conn
        .query_row(
            "SELECT count(*) FROM definitions WHERE name='recovered'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(recovered, 1);
    assert_eq!(retained_file_count(&cache), 0);
}

#[test]
fn missing_completed_file_column_is_discarded_and_explicit_index_recovers() {
    let (_directory, root, cache, mut store) = fixture();
    fs::write(root.join("sample.rs"), "pub fn recovered() {}\n").unwrap();
    drop(retain_fixture_sources(&store, &root, &cache));

    let stage = cache.join(CHECKPOINT_DIRECTORY).join(CHECKPOINT_DATABASE);
    Connection::open(&stage)
        .unwrap()
        .execute_batch(
            "DROP TABLE completed_files;
             CREATE TABLE completed_files(path TEXT PRIMARY KEY, facts TEXT NOT NULL);
             INSERT INTO completed_files(path, facts) VALUES('sample.rs', '{}');",
        )
        .unwrap();
    assert_eq!(retained_file_count(&cache), 1);

    let coverage = store.index().unwrap();
    assert_eq!(coverage.total_files, 1);
    let recovered: i64 = store
        .conn
        .query_row(
            "SELECT count(*) FROM definitions WHERE name='recovered'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(recovered, 1);
    assert_eq!(retained_file_count(&cache), 0);
}

#[test]
fn committed_extraction_survives_mark_seen_failure_and_bounded_retry() {
    let (_directory, root, cache, store) = fixture();
    let source = b"pub fn retry_after_mark_seen() {}\n";
    fs::write(root.join("sample.rs"), source).unwrap();
    let path = crate::path_codec::encode_path(Path::new("sample.rs"));
    let revision = blake3::hash(source).to_hex().to_string();
    let mut extraction = tp_extract::extract(&path, source);
    extraction.status = "recovered".to_owned();
    let budget = QueryDeadline::start();
    let mut checkpoint = IndexCheckpoint::open(&store.conn, &root, &cache, Some(&budget)).unwrap();
    checkpoint.begin_pass().unwrap();
    checkpoint
        .storage
        .as_ref()
        .unwrap()
        .fail_next_mark_seen_for_test();

    let error = checkpoint
        .completed_file(
            &path,
            Some(&revision),
            "read",
            source.len() as u64,
            crate::store::scan::extraction_cache_version(),
            &extraction,
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected checkpoint mark_seen failure")
    );
    assert_eq!(retained_file_count(&cache), 1);
    assert_eq!(
        checkpoint
            .storage
            .as_ref()
            .unwrap()
            .completed_count()
            .unwrap(),
        1
    );
    assert_eq!(checkpoint.progress.completed_files, 1);
    assert!(!checkpoint.maintenance_required());
    if let Some(progress) = crate::store::read_index_progress(&root, &cache) {
        assert!(!progress.maintenance_required);
    }
    drop(checkpoint);

    let mut retry = crate::store::Store::open(&root, &cache).unwrap();
    let coverage = retry
        .index_available_until(QueryDeadline::after(Duration::from_secs(2)))
        .unwrap()
        .expect("a fresh bounded request reuses the committed extraction and publishes");
    assert_eq!(coverage.total_files, 1);
    let recovered: i64 = retry
        .conn
        .query_row(
            "SELECT count(*) FROM definitions WHERE name='retry_after_mark_seen'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(recovered, 1);
    let retained_status: String = retry
        .conn
        .query_row("SELECT status FROM files WHERE path=?1", [&path], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(retained_status, "recovered");
    assert_eq!(retry.generation().unwrap(), 1);
    assert_eq!(retained_file_count(&cache), 0);
    assert!(crate::store::read_index_progress(&root, &cache).is_none());
}

#[test]
fn expired_setup_and_tiny_active_slices_do_not_poison_bounded_retries() {
    const FILES: usize = 4;
    for active_slice in [false, true] {
        let (_directory, root, cache, store) = fixture();
        for number in 0..FILES {
            fs::write(
                root.join(format!("file-{number:04}.txt")),
                format!("A retryable tiny-slice observation, file {number}.\n"),
            )
            .unwrap();
        }

        // A tiny active slice opens its checkpoint with a deadline too short to be eligible.
        let budget = QueryDeadline::start();
        let tiny_slice = QueryDeadline::after(Duration::from_millis(200));
        let opening = if active_slice { &tiny_slice } else { &budget };
        let mut checkpoint =
            IndexCheckpoint::open(&store.conn, &root, &cache, Some(opening)).unwrap();
        checkpoint.begin_pass().unwrap();
        let deadline = tiny_slice;
        std::thread::sleep(deadline.remaining() + Duration::from_millis(10));
        assert!(deadline.time_expired(), "the tiny deadline must expire");
        checkpoint.note_budget_expired(&deadline).unwrap();
        assert!(!checkpoint.maintenance_required());
        let progress = crate::store::read_index_progress(&root, &cache)
            .expect("the expired interruption persists progress");
        assert!(!progress.maintenance_required);
        drop(checkpoint);
        drop(store);

        let mut retry = crate::store::Store::open(&root, &cache).unwrap();
        let coverage = retry
            .index_available_until(QueryDeadline::after(Duration::from_secs(2)))
            .unwrap()
            .expect("a fresh bounded two-second request publishes the small fixture");
        assert_eq!(coverage.total_files, FILES);
        assert_eq!(retry.generation().unwrap(), 1);
    }
}

#[test]
fn bounded_attempt_removes_obsolete_stage_directories() {
    let (_directory, root, cache, mut store) = fixture();
    fs::write(root.join("sample.rs"), "pub fn staged() {}\n").unwrap();
    let obsolete = cache.join("stage-abandoned");
    fs::create_dir(&obsolete).unwrap();
    fs::write(obsolete.join("partial.sqlite3"), "incomplete").unwrap();

    assert!(
        store
            .index_available_until(QueryDeadline::after(Duration::from_secs(2)))
            .unwrap()
            .is_some()
    );
    assert!(!obsolete.exists());
    assert_eq!(store.generation().unwrap(), 1);
}

#[test]
fn slow_setup_before_checkpoint_open_still_counts_an_eligible_stall() {
    let (_directory, root, cache, store) = fixture();
    for number in 0..4 {
        fs::write(
            root.join(format!("file-{number}.txt")),
            format!("A slow-setup observation, file {number}.\n"),
        )
        .unwrap();
    }
    // Two seconds leaves the slice eligible at open, but 300 ms of setup
    // before the first pass would make it ineligible if the clock started later.
    let deadline = QueryDeadline::after(Duration::from_secs(2));
    let mut checkpoint =
        IndexCheckpoint::open(&store.conn, &root, &cache, Some(&deadline)).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    checkpoint.begin_pass().unwrap();
    std::thread::sleep(deadline.remaining() + Duration::from_millis(10));
    assert!(deadline.time_expired(), "the slice deadline must expire");
    checkpoint.note_budget_expired(&deadline).unwrap();
    assert_eq!(checkpoint.stalled_slices, 1);
    assert_eq!(progress_io::read_record(&cache).unwrap().stalled_slices, 1);
}

#[test]
fn commit_suspend_keeps_busy_timeout_within_slice_deadline() {
    fn busy_timeout(connection: &Connection) -> i64 {
        connection
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap()
    }
    let connection = Connection::open_in_memory().unwrap();
    let fresh = QueryDeadline::start();
    index_deadline::suspend_for_commit(&connection, &fresh).unwrap();
    assert_eq!(busy_timeout(&connection), 2_000);
    // An expired slice waits on nothing; clear() would restore a 30 s timeout here.
    let exhausted = QueryDeadline::after(Duration::from_millis(50));
    std::thread::sleep(Duration::from_millis(100));
    assert!(exhausted.time_expired());
    index_deadline::suspend_for_commit(&connection, &exhausted).unwrap();
    assert_eq!(busy_timeout(&connection), 0);
}

#[test]
fn open_clamps_completed_files_to_retained_count() {
    let (_directory, root, cache, store) = fixture();
    fs::write(root.join("sample.rs"), "pub fn clamped() {}\n").unwrap();
    drop(retain_fixture_sources(&store, &root, &cache));
    assert_eq!(retained_file_count(&cache), 1);
    // A crash can persist progress past the last batch commit.
    let mut record = progress_io::read_record(&cache).expect("a retained pass writes progress");
    record.progress.completed_files = 41;
    progress_io::write(&cache, &record).unwrap();
    let budget = QueryDeadline::start();
    let checkpoint = IndexCheckpoint::open(&store.conn, &root, &cache, Some(&budget)).unwrap();
    assert_eq!(checkpoint.progress.completed_files, 1);
}

#[test]
fn unbounded_pass_resets_stalled_slices() {
    let (_directory, root, cache, store) = fixture();
    fs::write(root.join("sample.rs"), "pub fn maintained() {}\n").unwrap();
    drop(retain_fixture_sources(&store, &root, &cache));
    let budget = QueryDeadline::start();
    let mut stalled = IndexCheckpoint::open(&store.conn, &root, &cache, Some(&budget)).unwrap();
    stalled.begin_pass().unwrap();
    for _ in 0..STALLED_SLICES_BEFORE_MAINTENANCE {
        stalled.note_stalled_slice().unwrap();
    }
    assert!(stalled.maintenance_required());
    assert_eq!(stalled.stalled_slices, STALLED_SLICES_BEFORE_MAINTENANCE);
    drop(stalled);
    let mut checkpoint = IndexCheckpoint::open(&store.conn, &root, &cache, None).unwrap();
    assert!(checkpoint.has_retained_facts());
    assert_eq!(checkpoint.stalled_slices, STALLED_SLICES_BEFORE_MAINTENANCE);
    checkpoint.begin_pass().unwrap();
    assert_eq!(checkpoint.stalled_slices, 0);
    assert!(!checkpoint.maintenance_required());
}
