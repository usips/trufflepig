use crate::workspace::{hashed_member_cache, test_fixture::Fixture};
use fs2::FileExt;
use std::{
    fs::{self, OpenOptions},
    time::{Duration, Instant},
};

/// Inline workspace reads keep a published parent answer while the worktree writer is held.
#[test]
fn actual_no_daemon_worktree_read_serves_parent_hit_promptly_under_held_lease() {
    let fixture = Fixture::git_backed();
    let parent = fixture.root.path().join("engine");
    let parent_cache = fixture.member_cache("engine");
    crate::store::Store::open(&parent, &parent_cache)
        .unwrap()
        .index()
        .unwrap();

    let elsewhere = tempfile::tempdir().unwrap();
    let worktree = fixture.worktree("engine", &elsewhere.path().join("wt-held-lease"));
    fs::write(
        worktree.join("lib.rs"),
        "pub struct SharedThing;\npub fn worktree_only() {}\n",
    )
    .unwrap();
    let cache = hashed_member_cache(&worktree, Some(fixture.cache.path())).unwrap();
    fs::create_dir_all(&cache).unwrap();
    let lease = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join("index.lock"))
        .unwrap();
    lease.lock_exclusive().unwrap();

    let _budget = crate::output::OutputBudget::new(600).unwrap();
    let started = Instant::now();
    let output = fixture
        .run_at(
            &worktree,
            &["--format", "json", "search", "sym:SharedThing"],
        )
        .unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "held-lease parent fallback took {elapsed:?}: {output}"
    );
    let response: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(response["hits"][0]["name"], "SharedThing", "{response}");
    assert!(
        response["coverage"].as_array().is_some_and(|coverage| {
            coverage.iter().any(|row| {
                row["state"] == "parent_fallback"
                    && row["served_from"]
                        .as_str()
                        .is_some_and(|value| value.contains("engine index"))
            })
        }),
        "{response}"
    );
    assert_unpublished_index(&worktree, &cache);
    drop(lease);
}

fn assert_unpublished_index(root: &std::path::Path, cache: &std::path::Path) {
    let index = cache.join("index.sqlite3");
    assert!(
        index.is_file(),
        "reconcile attempt should initialize the index schema"
    );
    let connection =
        rusqlite::Connection::open_with_flags(&index, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let generation: String = connection
        .query_row("SELECT value FROM meta WHERE key='generation'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(generation, "0", "held lease must not publish a generation");
    let publication_count: i64 = connection
        .query_row("SELECT count(*) FROM publication_windows", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        publication_count, 0,
        "held lease must not record publication"
    );

    let error = match crate::store::Store::open_read(
        root,
        cache,
        crate::daemon::deadline::QueryDeadline::start(),
    ) {
        Ok(_) => panic!("generation-zero index must be warming"),
        Err(error) => error,
    };
    assert!(crate::store::is_index_warming(&error), "{error:#}");
}
