use super::seed::{SeedOutcome, ensure_seeded, ensure_seeded_with_limit};
use super::{Store, encode_path};
use crate::extract::{Definition, Extraction};
use rusqlite::{Connection, params};
use std::fs;
use std::path::{Path, PathBuf};

struct SeedFixture {
    _directory: tempfile::TempDir,
    member_root: PathBuf,
    member_cache: PathBuf,
    worktree_root: PathBuf,
    worktree_cache: PathBuf,
}

/// A published member store and an identical, unindexed worktree beside it.
fn fixture() -> SeedFixture {
    let directory = tempfile::tempdir().unwrap();
    let member_root = directory.path().join("member");
    let worktree_root = directory.path().join("worktree");
    for root in [&member_root, &worktree_root] {
        fs::create_dir(root).unwrap();
        fs::write(root.join("lib.rs"), "fn seeded_value() {}\n").unwrap();
        fs::write(root.join("notes.md"), "seed notes\n").unwrap();
    }
    let member_cache = directory.path().join("member-cache");
    let worktree_cache = directory.path().join("worktree-cache");
    Store::open(&member_root, &member_cache)
        .unwrap()
        .index()
        .unwrap();
    SeedFixture {
        _directory: directory,
        member_root,
        member_cache,
        worktree_root,
        worktree_cache,
    }
}

fn meta(cache: &Path, key: &str) -> String {
    Connection::open(cache.join("index.sqlite3"))
        .unwrap()
        .query_row("SELECT value FROM meta WHERE key=?1", [key], |row| {
            row.get(0)
        })
        .unwrap()
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

/// Adds a definition named `seeded_marker` to the member's cached Rust facts.
fn plant_marker(member_cache: &Path) {
    let conn = Connection::open(member_cache.join("index.sqlite3")).unwrap();
    let (revision, version, facts): (String, i64, String) = conn
        .query_row(
            "SELECT revision,version,facts FROM extraction_cache WHERE grammar='rs'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let mut extraction: Extraction = serde_json::from_str(&facts).unwrap();
    extraction.definitions.push(Definition {
        name: "seeded_marker".into(),
        kind: "function".into(),
        start: 0,
        end: 2,
        container: None,
    });
    conn.execute(
        "UPDATE extraction_cache SET facts=?1 WHERE revision=?2 AND grammar='rs' AND version=?3",
        params![
            serde_json::to_string(&extraction).unwrap(),
            revision,
            version
        ],
    )
    .unwrap();
}

fn seed_leftovers(cache: &Path) -> Vec<String> {
    fs::read_dir(cache)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|name| name.contains(".seed"))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn seeded_store_has_no_publication_until_indexed() {
    let fixture = fixture();
    let source_rows = count(
        &Connection::open(fixture.member_cache.join("index.sqlite3")).unwrap(),
        "SELECT count(*) FROM extraction_cache",
    );
    assert!(source_rows >= 1);
    let outcome = ensure_seeded(
        &fixture.member_cache,
        &fixture.worktree_root,
        &fixture.worktree_cache,
    );
    assert_eq!(
        outcome,
        SeedOutcome::Seeded {
            extraction_rows: source_rows as u64,
            embeddings: false
        }
    );
    let store = Store::open(&fixture.worktree_root, &fixture.worktree_cache).unwrap();
    assert_eq!(store.generation().unwrap(), 0);
    assert!(store.publication().unwrap().is_none());
    assert_eq!(
        meta(&fixture.worktree_cache, "root"),
        encode_path(&fixture.worktree_root.canonicalize().unwrap())
    );
    assert_ne!(
        meta(&fixture.worktree_cache, "index_epoch"),
        meta(&fixture.member_cache, "index_epoch")
    );
    assert_eq!(
        count(&store.conn, "SELECT count(*) FROM extraction_cache"),
        source_rows
    );
    assert_eq!(count(&store.conn, "SELECT count(*) FROM files"), 0);
    assert!(
        !fixture.worktree_cache.join("preparation.sqlite3").exists()
            && seed_leftovers(&fixture.worktree_cache).is_empty()
    );
}

#[test]
fn seeded_identical_tree_reuses_extraction_facts() {
    let fixture = fixture();
    plant_marker(&fixture.member_cache);
    ensure_seeded(
        &fixture.member_cache,
        &fixture.worktree_root,
        &fixture.worktree_cache,
    );
    let mut store = Store::open(&fixture.worktree_root, &fixture.worktree_cache).unwrap();
    store.index().unwrap();
    assert_eq!(
        count(
            &store.conn,
            "SELECT count(*) FROM definitions WHERE name='seeded_marker'"
        ),
        1
    );
    let publication = store.publication().unwrap().unwrap();
    assert_eq!(publication.generation, 1);
    assert_eq!(
        publication.root,
        encode_path(&fixture.worktree_root.canonicalize().unwrap())
    );
}

#[test]
fn seeded_modified_file_reextracts() {
    let fixture = fixture();
    plant_marker(&fixture.member_cache);
    fs::write(
        fixture.worktree_root.join("lib.rs"),
        "fn seeded_value() {}\nfn worktree_only() {}\n",
    )
    .unwrap();
    ensure_seeded(
        &fixture.member_cache,
        &fixture.worktree_root,
        &fixture.worktree_cache,
    );
    let mut store = Store::open(&fixture.worktree_root, &fixture.worktree_cache).unwrap();
    store.index().unwrap();
    assert_eq!(
        count(
            &store.conn,
            "SELECT count(*) FROM definitions WHERE name='seeded_marker'"
        ),
        0
    );
    assert_eq!(
        count(
            &store.conn,
            "SELECT count(*) FROM definitions WHERE name='worktree_only'"
        ),
        1
    );
}

#[test]
fn seed_copies_embeddings_by_content_key() {
    let fixture = fixture();
    let embeddings = Connection::open(fixture.member_cache.join("embeddings.sqlite")).unwrap();
    embeddings
        .execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA user_version=2;
             CREATE TABLE embeddings(key TEXT PRIMARY KEY, vector BLOB NOT NULL, touched INTEGER NOT NULL);
             INSERT INTO embeddings VALUES('content:seeded', x'0102', 1);",
        )
        .unwrap();
    fs::write(fixture.member_cache.join("preparation.sqlite3"), b"").unwrap();
    let outcome = ensure_seeded(
        &fixture.member_cache,
        &fixture.worktree_root,
        &fixture.worktree_cache,
    );
    assert!(matches!(
        outcome,
        SeedOutcome::Seeded {
            embeddings: true,
            ..
        }
    ));
    let copy = Connection::open(fixture.worktree_cache.join("embeddings.sqlite")).unwrap();
    let version: i64 = copy
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 2);
    assert_eq!(
        count(
            &copy,
            "SELECT count(*) FROM embeddings WHERE key='content:seeded'"
        ),
        1
    );
    assert!(!fixture.worktree_cache.join("preparation.sqlite3").exists());
    assert!(seed_leftovers(&fixture.worktree_cache).is_empty());
}

#[test]
fn seed_succeeds_under_live_source_writer() {
    let fixture = fixture();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(fixture.member_cache.join("index.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    let writer = Store::open(&fixture.member_root, &fixture.member_cache).unwrap();
    writer.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    let outcome = ensure_seeded(
        &fixture.member_cache,
        &fixture.worktree_root,
        &fixture.worktree_cache,
    );
    assert!(matches!(outcome, SeedOutcome::Seeded { .. }), "{outcome:?}");
    writer.conn.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        Store::open(&fixture.worktree_root, &fixture.worktree_cache)
            .unwrap()
            .generation()
            .unwrap(),
        0
    );
}

#[test]
fn seed_skips_oversize_source_and_is_idempotent() {
    let fixture = fixture();
    let outcome = ensure_seeded_with_limit(
        &fixture.member_cache,
        &fixture.worktree_root,
        &fixture.worktree_cache,
        1,
    );
    assert_eq!(
        outcome,
        SeedOutcome::Skipped("source index exceeds seed limit")
    );
    assert!(!fixture.worktree_cache.join("index.sqlite3").exists());
    assert!(seed_leftovers(&fixture.worktree_cache).is_empty());
    assert!(matches!(
        ensure_seeded(
            &fixture.member_cache,
            &fixture.worktree_root,
            &fixture.worktree_cache
        ),
        SeedOutcome::Seeded { .. }
    ));
    assert_eq!(
        ensure_seeded(
            &fixture.member_cache,
            &fixture.worktree_root,
            &fixture.worktree_cache
        ),
        SeedOutcome::Present
    );
    assert!(seed_leftovers(&fixture.worktree_cache).is_empty());
}
