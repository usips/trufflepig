use super::*;
use fs2::FileExt;
use rusqlite::Connection;
use std::{
    fs::{self, OpenOptions},
    path::PathBuf,
    time::{Duration, Instant},
};

struct SeedMemoFixture {
    _directory: tempfile::TempDir,
    member_root: PathBuf,
    member_cache: PathBuf,
    worktree_root: PathBuf,
    worktree_cache: PathBuf,
}

fn fixture() -> SeedMemoFixture {
    let directory = tempfile::tempdir().unwrap();
    let member_root = directory.path().join("member");
    let worktree_root = directory.path().join("worktree");
    fs::create_dir(&member_root).unwrap();
    fs::create_dir(&worktree_root).unwrap();
    fs::write(member_root.join("lib.rs"), "fn seeded_value() {}\n").unwrap();
    fs::write(worktree_root.join("lib.rs"), "fn seeded_value() {}\n").unwrap();
    let member_cache = directory.path().join("member-cache");
    fs::create_dir(&member_cache).unwrap();
    let worktree_cache = directory.path().join("worktree-cache");
    SeedMemoFixture {
        _directory: directory,
        member_root,
        member_cache,
        worktree_root,
        worktree_cache,
    }
}

fn publish_member_index(fixture: &SeedMemoFixture) {
    super::super::Store::open(&fixture.member_root, &fixture.member_cache)
        .unwrap()
        .index()
        .unwrap();
}

fn hold_writer_lease(cache: &std::path::Path) -> std::fs::File {
    fs::create_dir_all(cache).unwrap();
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join("index.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();
    lock
}

fn query_seed(fixture: &SeedMemoFixture) -> SeedOutcome {
    ensure_seeded_until(
        &fixture.member_cache,
        &fixture.worktree_root,
        &fixture.worktree_cache,
        &QueryDeadline::after(Duration::from_secs(2)),
    )
}

#[test]
fn repeated_writer_lease_skip_uses_the_saved_memo() {
    let fixture = fixture();
    publish_member_index(&fixture);
    let lock = hold_writer_lease(&fixture.worktree_cache);

    assert_eq!(
        query_seed(&fixture),
        SeedOutcome::Skipped("index writer lease is held")
    );
    let outcome_path = fixture.worktree_cache.join(OUTCOME_NAME);
    let first_record = fs::read(&outcome_path).unwrap();
    assert!(read_seed_outcome(&fixture.worktree_cache).unwrap()["memo"].is_object());
    drop(lock);

    assert_eq!(
        query_seed(&fixture),
        SeedOutcome::Skipped("index writer lease is held")
    );
    assert!(!fixture.worktree_cache.join(INDEX_NAME).exists());
    assert_eq!(fs::read(outcome_path).unwrap(), first_record);
}

#[test]
fn capped_seed_expiry_is_memoized_while_the_original_caller_lives() {
    let fixture = fixture();
    publish_member_index(&fixture);
    let source_index = fixture.member_cache.join(INDEX_NAME);
    let locked_source = Connection::open(&source_index).unwrap();
    locked_source.busy_timeout(Duration::ZERO).unwrap();
    locked_source
        .execute_batch("PRAGMA journal_mode=DELETE; BEGIN EXCLUSIVE;")
        .unwrap();
    let caller_deadline = QueryDeadline::after(Duration::from_secs(2));
    let identity = SeedMemoIdentity::capture(
        &fixture.member_cache,
        &fixture.worktree_root,
        &fixture.worktree_cache,
        &caller_deadline,
    )
    .unwrap();

    let started = Instant::now();
    let outcome = ensure_seeded_recorded(
        &fixture.member_cache,
        &fixture.worktree_root,
        &fixture.worktree_cache,
        MAX_SEED_SOURCE_BYTES,
        QueryDeadline::after(Duration::from_millis(40)),
        Some(&identity),
        Some(&caller_deadline),
    );
    assert_eq!(outcome, SeedOutcome::Skipped("seed failed"));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(!caller_deadline.expired());
    let outcome_path = fixture.worktree_cache.join(OUTCOME_NAME);
    let first_record = fs::read(&outcome_path).unwrap();
    let diagnostic = read_seed_outcome(&fixture.worktree_cache).unwrap();
    assert!(
        diagnostic["memo"]["created_ms"].as_u64().unwrap()
            > diagnostic["recorded_ms"].as_u64().unwrap()
    );

    let retry_started = Instant::now();
    assert_eq!(query_seed(&fixture), SeedOutcome::Skipped("seed failed"));
    assert!(retry_started.elapsed() < Duration::from_millis(250));
    assert_eq!(fs::read(outcome_path).unwrap(), first_record);
    drop(locked_source);
}

#[test]
fn expired_original_caller_does_not_receive_a_skipped_memo() {
    let fixture = fixture();
    let capture_deadline = QueryDeadline::after(Duration::from_secs(1));
    let identity = SeedMemoIdentity::capture(
        &fixture.member_cache,
        &fixture.worktree_root,
        &fixture.worktree_cache,
        &capture_deadline,
    )
    .unwrap();
    let expired_caller = QueryDeadline::after(Duration::ZERO);

    assert_eq!(
        ensure_seeded_recorded(
            &fixture.member_cache,
            &fixture.worktree_root,
            &fixture.worktree_cache,
            MAX_SEED_SOURCE_BYTES,
            QueryDeadline::after(Duration::from_secs(1)),
            Some(&identity),
            Some(&expired_caller),
        ),
        SeedOutcome::Skipped("member cache has no index")
    );
    assert!(
        read_seed_outcome(&fixture.worktree_cache)
            .unwrap()
            .get("memo")
            .is_none()
    );
}

#[test]
fn member_index_publication_invalidates_an_absent_source_memo() {
    let fixture = fixture();

    assert_eq!(
        query_seed(&fixture),
        SeedOutcome::Skipped("member cache has no index")
    );
    assert!(read_seed_outcome(&fixture.worktree_cache).unwrap()["memo"].is_object());
    publish_member_index(&fixture);

    assert!(matches!(query_seed(&fixture), SeedOutcome::Seeded { .. }));
    assert!(fixture.worktree_cache.join(INDEX_NAME).is_file());
}

#[test]
fn old_skipped_diagnostic_without_identity_does_not_suppress_seeding() {
    let fixture = fixture();
    publish_member_index(&fixture);
    let lock = hold_writer_lease(&fixture.worktree_cache);

    assert_eq!(
        ensure_seeded(
            &fixture.member_cache,
            &fixture.worktree_root,
            &fixture.worktree_cache,
        ),
        SeedOutcome::Skipped("index writer lease is held")
    );
    assert!(
        read_seed_outcome(&fixture.worktree_cache)
            .unwrap()
            .get("memo")
            .is_none()
    );
    drop(lock);

    assert!(matches!(query_seed(&fixture), SeedOutcome::Seeded { .. }));
}

#[test]
fn memo_is_bound_to_the_member_cache_path() {
    let fixture = fixture();
    publish_member_index(&fixture);
    let alternate_cache = fixture._directory.path().join("alternate-member-cache");
    fs::create_dir(&alternate_cache).unwrap();
    fs::hard_link(
        fixture.member_cache.join(INDEX_NAME),
        alternate_cache.join(INDEX_NAME),
    )
    .unwrap();
    let lock = hold_writer_lease(&fixture.worktree_cache);

    assert_eq!(
        query_seed(&fixture),
        SeedOutcome::Skipped("index writer lease is held")
    );
    drop(lock);

    let outcome = ensure_seeded_until(
        &alternate_cache,
        &fixture.worktree_root,
        &fixture.worktree_cache,
        &QueryDeadline::after(Duration::from_secs(2)),
    );
    assert!(matches!(outcome, SeedOutcome::Seeded { .. }));
}

#[test]
fn memo_is_bound_to_the_worktree_root() {
    let fixture = fixture();
    publish_member_index(&fixture);
    let alternate_root = fixture._directory.path().join("alternate-worktree");
    fs::create_dir(&alternate_root).unwrap();
    let lock = hold_writer_lease(&fixture.worktree_cache);

    assert_eq!(
        query_seed(&fixture),
        SeedOutcome::Skipped("index writer lease is held")
    );
    drop(lock);

    let outcome = ensure_seeded_until(
        &fixture.member_cache,
        &alternate_root,
        &fixture.worktree_cache,
        &QueryDeadline::after(Duration::from_secs(2)),
    );
    assert!(matches!(outcome, SeedOutcome::Seeded { .. }));
    let stored_root: String = Connection::open(fixture.worktree_cache.join(INDEX_NAME))
        .unwrap()
        .query_row("SELECT value FROM meta WHERE key='root'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        stored_root,
        encode_path(&alternate_root.canonicalize().unwrap())
    );
}

#[test]
fn memo_is_bound_to_the_worktree_cache_path() {
    let fixture = fixture();
    publish_member_index(&fixture);
    let alternate_cache = fixture._directory.path().join("alternate-worktree-cache");
    let lock = hold_writer_lease(&fixture.worktree_cache);

    assert_eq!(
        query_seed(&fixture),
        SeedOutcome::Skipped("index writer lease is held")
    );
    let diagnostic = fixture.worktree_cache.join(OUTCOME_NAME);
    drop(lock);
    fs::create_dir(&alternate_cache).unwrap();
    fs::copy(diagnostic, alternate_cache.join(OUTCOME_NAME)).unwrap();

    let outcome = ensure_seeded_until(
        &fixture.member_cache,
        &fixture.worktree_root,
        &alternate_cache,
        &QueryDeadline::after(Duration::from_secs(2)),
    );
    assert!(matches!(outcome, SeedOutcome::Seeded { .. }));
}
