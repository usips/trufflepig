use super::*;
use fs2::FileExt;
use rusqlite::Connection;
use std::{
    fs::{self, OpenOptions},
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

struct SeedDeadlineFixture {
    _directory: tempfile::TempDir,
    member_cache: PathBuf,
    worktree_root: PathBuf,
    worktree_cache: PathBuf,
}

fn fixture() -> SeedDeadlineFixture {
    let directory = tempfile::tempdir().unwrap();
    let member_root = directory.path().join("member");
    let worktree_root = directory.path().join("worktree");
    fs::create_dir(&member_root).unwrap();
    fs::create_dir(&worktree_root).unwrap();
    fs::write(member_root.join("lib.rs"), "fn seeded_value() {}\n").unwrap();
    fs::write(worktree_root.join("lib.rs"), "fn seeded_value() {}\n").unwrap();
    let member_cache = directory.path().join("member-cache");
    let worktree_cache = directory.path().join("worktree-cache");
    super::super::Store::open(&member_root, &member_cache)
        .unwrap()
        .index()
        .unwrap();
    SeedDeadlineFixture {
        _directory: directory,
        member_cache,
        worktree_root,
        worktree_cache,
    }
}

fn count_extractions(cache: &std::path::Path) -> i64 {
    Connection::open(cache.join(INDEX_NAME))
        .unwrap()
        .query_row("SELECT count(*) FROM extraction_cache", [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[test]
fn held_writer_lease_skips_without_touching_active_seed_files() {
    let fixture = fixture();
    fs::create_dir_all(&fixture.worktree_cache).unwrap();
    let staged = seed_path(&fixture.worktree_cache, INDEX_NAME);
    let staged_journal = journal_path(&staged);
    let staged_bytes = b"active seed writer bytes";
    let journal_bytes = b"active seed journal bytes";
    fs::write(&staged, staged_bytes).unwrap();
    fs::write(&staged_journal, journal_bytes).unwrap();

    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(fixture.worktree_cache.join("index.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();

    let member_cache = fixture.member_cache.clone();
    let worktree_root = fixture.worktree_root.clone();
    let worktree_cache = fixture.worktree_cache.clone();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        sender
            .send(ensure_seeded(
                &member_cache,
                &worktree_root,
                &worktree_cache,
            ))
            .unwrap();
    });
    let outcome = receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("seed attempt waited on the held writer lease");

    assert_eq!(outcome, SeedOutcome::Skipped("index writer lease is held"));
    assert_eq!(fs::read(&staged).unwrap(), staged_bytes);
    assert_eq!(fs::read(&staged_journal).unwrap(), journal_bytes);
    let diagnostic = read_seed_outcome(&fixture.worktree_cache).unwrap();
    assert_eq!(diagnostic["outcome"], "skipped");
    assert_eq!(diagnostic["reason"], "index writer lease is held");
    drop(lock);
}

#[test]
fn expired_seed_deadline_cleans_staging_without_publishing() {
    let fixture = fixture();
    fs::create_dir_all(&fixture.worktree_cache).unwrap();
    let staged = seed_path(&fixture.worktree_cache, INDEX_NAME);
    let staged_journal = journal_path(&staged);
    fs::write(&staged, b"stale staged index").unwrap();
    fs::write(&staged_journal, b"stale staged journal").unwrap();
    let mut record = serde_json::json!({"source": encode_path(&fixture.member_cache)});

    let result = seed(
        &fixture.member_cache,
        &fixture.worktree_root,
        &fixture.worktree_cache,
        MAX_SEED_SOURCE_BYTES,
        QueryDeadline::after(Duration::ZERO),
        &mut record,
    );

    assert!(result.is_err());
    assert!(!fixture.worktree_cache.join(INDEX_NAME).exists());
    assert!(!staged.exists());
    assert!(!staged_journal.exists());
}

#[test]
fn busy_source_aborts_seed_within_the_shared_deadline() {
    let fixture = fixture();
    let source_index = fixture.member_cache.join(INDEX_NAME);
    let locked_source = Connection::open(&source_index).unwrap();
    locked_source.busy_timeout(Duration::ZERO).unwrap();
    locked_source
        .execute_batch("PRAGMA journal_mode=DELETE; BEGIN EXCLUSIVE;")
        .unwrap();

    let started = Instant::now();
    let outcome = ensure_seeded_with_deadline(
        &fixture.member_cache,
        &fixture.worktree_root,
        &fixture.worktree_cache,
        MAX_SEED_SOURCE_BYTES,
        QueryDeadline::after(Duration::from_millis(40)),
    );

    assert_eq!(outcome, SeedOutcome::Skipped("seed failed"));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!fixture.worktree_cache.join(INDEX_NAME).exists());
    assert!(!seed_path(&fixture.worktree_cache, INDEX_NAME).exists());
    assert!(!journal_path(&seed_path(&fixture.worktree_cache, INDEX_NAME)).exists());
    let diagnostic = read_seed_outcome(&fixture.worktree_cache).unwrap();
    assert_eq!(diagnostic["outcome"], "skipped");
    let error = diagnostic["error"].as_str().unwrap();
    assert!(error.to_ascii_lowercase().contains("locked"), "{error}");
    drop(locked_source);
}

#[test]
fn expired_caller_seed_deadline_creates_no_cache_files() {
    let fixture = fixture();
    let deadline = QueryDeadline::after(Duration::ZERO);

    let outcome = ensure_seeded_until(
        &fixture.member_cache,
        &fixture.worktree_root,
        &fixture.worktree_cache,
        &deadline,
    );

    assert_eq!(outcome, SeedOutcome::Skipped("seed deadline expired"));
    assert!(!fixture.worktree_cache.exists());
}

#[test]
fn removed_claim_cancels_seed_before_cache_io() {
    let fixture = fixture();
    let claim = fixture._directory.path().join("seed.claim");
    fs::write(&claim, "claimed").unwrap();
    let deadline = QueryDeadline::after(Duration::from_secs(5)).with_claim_path(claim.clone());
    fs::remove_file(claim).unwrap();

    let outcome = ensure_seeded_until(
        &fixture.member_cache,
        &fixture.worktree_root,
        &fixture.worktree_cache,
        &deadline,
    );

    assert_eq!(outcome, SeedOutcome::Skipped("seed deadline expired"));
    assert!(!fixture.worktree_cache.exists());
}

#[test]
fn caller_deadline_bounds_a_locked_source_seed() {
    let fixture = fixture();
    let source_index = fixture.member_cache.join(INDEX_NAME);
    let locked_source = Connection::open(&source_index).unwrap();
    locked_source.busy_timeout(Duration::ZERO).unwrap();
    locked_source
        .execute_batch("PRAGMA journal_mode=DELETE; BEGIN EXCLUSIVE;")
        .unwrap();

    let deadline = QueryDeadline::after(Duration::from_millis(100));
    let started = Instant::now();
    let outcome = ensure_seeded_until(
        &fixture.member_cache,
        &fixture.worktree_root,
        &fixture.worktree_cache,
        &deadline,
    );

    assert_eq!(outcome, SeedOutcome::Skipped("seed failed"));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(!fixture.worktree_cache.join(INDEX_NAME).exists());
    drop(locked_source);
}

#[test]
fn small_valid_member_seed_is_published_with_extraction_facts() {
    let fixture = fixture();
    let source_rows = count_extractions(&fixture.member_cache);
    assert!(source_rows > 0);

    let outcome = ensure_seeded(
        &fixture.member_cache,
        &fixture.worktree_root,
        &fixture.worktree_cache,
    );
    let diagnostic = read_seed_outcome(&fixture.worktree_cache).unwrap();

    assert_eq!(
        outcome,
        SeedOutcome::Seeded {
            extraction_rows: source_rows as u64,
            embeddings: false,
        },
        "seed diagnostic: {diagnostic}"
    );
    assert_eq!(diagnostic["outcome"], "seeded");
    assert_eq!(diagnostic["extraction_rows"], source_rows);
    assert_eq!(diagnostic["embeddings"], false);
    assert_eq!(count_extractions(&fixture.worktree_cache), source_rows);
    assert!(!seed_path(&fixture.worktree_cache, INDEX_NAME).exists());
}
