use super::{Fixture, coverage_line, search_without_waiting};
use crate::{
    search::InvocationDirectory,
    store::{Store, read_seed_outcome},
    workspace::{
        WorkspaceConfig, hashed_member_cache,
        home_index::{ParentRead, acquire_home_read},
        member_root::MemberRoot,
    },
};
use fs2::FileExt;
use std::{
    fs::{self, OpenOptions},
    os::unix::net::UnixStream,
    path::Path,
    sync::Arc,
    time::Duration,
};
use tp_core::{path_codec::encode_path, query_deadline::QueryDeadline};

#[test]
fn fresh_worktree_read_seeds_parent_facts_and_marks_cache_root() {
    let fixture = Fixture::git_backed();
    let parent = fixture.root.path().join("engine");
    let parent_cache = fixture.member_cache("engine");
    Store::open(&parent, &parent_cache)
        .unwrap()
        .index()
        .unwrap();

    let elsewhere = tempfile::tempdir().unwrap();
    let worktree = fixture.worktree("engine", &elsewhere.path().join("wt-seed"));
    fs::write(
        worktree.join("lib.rs"),
        "// read through parent index\npub struct SharedThing;\n",
    )
    .unwrap();
    let cache = hashed_member_cache(&worktree, Some(fixture.cache.path())).unwrap();
    assert!(
        !cache.exists(),
        "the search starts without a worktree cache"
    );

    let text = search_without_waiting(
        &fixture,
        &worktree,
        &["--format", "lines", "search", "SharedThing"],
    );
    assert!(
        coverage_line(&text).contains("served from engine index"),
        "{text}"
    );
    assert!(cache.is_dir(), "{cache:?}");
    assert_cache_root(&cache, &worktree);

    let seed = read_seed_outcome(&cache).expect("the seed attempt is recorded");
    assert_eq!(seed["outcome"], "seeded", "{seed}");
    assert!(
        seed["extraction_rows"]
            .as_u64()
            .is_some_and(|rows| rows > 0),
        "parent extraction facts were copied: {seed}"
    );
    let store = Store::open(&worktree, &cache).unwrap();
    assert_eq!(store.generation().unwrap(), 0);
    assert!(store.publication().unwrap().is_none());
}

#[test]
fn skipped_seed_still_marks_cache_used_for_parent_index_reads() {
    let fixture = Fixture::git_backed();
    let parent = fixture.root.path().join("engine");
    let parent_cache = fixture.member_cache("engine");
    Store::open(&parent, &parent_cache)
        .unwrap()
        .index()
        .unwrap();

    let elsewhere = tempfile::tempdir().unwrap();
    let worktree = fixture.worktree("engine", &elsewhere.path().join("wt-seed-skip"));
    fs::write(
        worktree.join("lib.rs"),
        "// parent fallback still answers\npub struct SharedThing;\n",
    )
    .unwrap();
    let cache = hashed_member_cache(&worktree, Some(fixture.cache.path())).unwrap();
    fs::create_dir_all(&cache).unwrap();
    let writer = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join("index.lock"))
        .unwrap();
    writer.lock_exclusive().unwrap();

    let text = search_without_waiting(
        &fixture,
        &worktree,
        &["--format", "lines", "search", "SharedThing"],
    );
    assert!(
        coverage_line(&text).contains("served from engine index"),
        "{text}"
    );
    assert!(!cache.join("index.sqlite3").exists());
    assert_cache_root(&cache, &worktree);

    let seed = read_seed_outcome(&cache).expect("the skipped seed attempt is recorded");
    assert_eq!(seed["outcome"], "skipped", "{seed}");
    assert_eq!(seed["reason"], "index writer lease is held", "{seed}");
}

#[test]
fn first_parent_show_publishes_own_index_but_keeps_parent_provenance() {
    let fixture = Fixture::git_backed();
    let parent = fixture.root.path().join("engine");
    let parent_cache = fixture.member_cache("engine");
    Store::open(&parent, &parent_cache)
        .unwrap()
        .index()
        .unwrap();

    let elsewhere = tempfile::tempdir().unwrap();
    let worktree = fixture.worktree("engine", &elsewhere.path().join("wt-show-publish"));
    fs::write(
        worktree.join("lib.rs"),
        "// changed after the parent index\npub struct SharedThing { pub current: bool }\n",
    )
    .unwrap();
    let cache = hashed_member_cache(&worktree, Some(fixture.cache.path())).unwrap();
    assert!(
        !cache.exists(),
        "the first read starts without a worktree cache"
    );

    let first: serde_json::Value = serde_json::from_str(
        &fixture
            .run_at(&worktree, &["show", "sym:SharedThing"])
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        first["served_from"], "engine index; re-extracted in worktree",
        "the source selected at request entry remains visible: {first}"
    );
    assert_eq!(
        first["lines"][0]["text"],
        "pub struct SharedThing { pub current: bool }"
    );
    assert_cache_root(&cache, &worktree);

    let own = Store::open(&worktree, &cache).unwrap();
    assert!(own.generation().unwrap() > 0);
    assert!(own.publication().unwrap().is_some());
    drop(own);

    let second: serde_json::Value = serde_json::from_str(
        &fixture
            .run_at(&worktree, &["show", "sym:SharedThing"])
            .unwrap(),
    )
    .unwrap();
    assert!(
        second["served_from"].is_null(),
        "the next read uses the own index: {second}"
    );
}

#[test]
fn held_worktree_writer_does_not_hide_a_usable_parent_show() {
    let fixture = Fixture::git_backed();
    let parent = fixture.root.path().join("engine");
    let parent_cache = fixture.member_cache("engine");
    Store::open(&parent, &parent_cache)
        .unwrap()
        .index()
        .unwrap();

    let elsewhere = tempfile::tempdir().unwrap();
    let worktree = fixture.worktree("engine", &elsewhere.path().join("wt-show-held"));
    let cache = hashed_member_cache(&worktree, Some(fixture.cache.path())).unwrap();
    fs::create_dir_all(&cache).unwrap();
    let writer = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join("index.lock"))
        .unwrap();
    writer.lock_exclusive().unwrap();

    let shown: serde_json::Value = serde_json::from_str(
        &fixture
            .run_at(&worktree, &["show", "sym:SharedThing"])
            .unwrap(),
    )
    .unwrap();
    assert_eq!(shown["served_from"], "engine index", "{shown}");
    assert_eq!(shown["verified"], true, "{shown}");
    assert_cache_root(&cache, &worktree);
    let seed = read_seed_outcome(&cache).expect("the skipped seed attempt is recorded");
    assert_eq!(seed["outcome"], "skipped", "{seed}");
    assert_eq!(seed["reason"], "index writer lease is held", "{seed}");
    let own = Store::open(&worktree, &cache).unwrap();
    assert_eq!(own.generation().unwrap(), 0);
    assert!(own.publication().unwrap().is_none());
}

#[test]
fn exhausted_shared_slice_keeps_parent_show_answer_without_own_index() {
    let fixture = Fixture::git_backed();
    let parent = fixture.root.path().join("engine");
    let parent_cache = fixture.member_cache("engine");
    Store::open(&parent, &parent_cache)
        .unwrap()
        .index()
        .unwrap();

    let elsewhere = tempfile::tempdir().unwrap();
    let worktree = fixture.worktree("engine", &elsewhere.path().join("wt-show-shared-slice"));
    fs::write(
        worktree.join("lib.rs"),
        "// changed after the parent index\npub struct SharedThing { pub current: bool }\n",
    )
    .unwrap();
    let config = WorkspaceConfig::load(&fixture.config).unwrap();
    let configured = config
        .members
        .iter()
        .find(|member| member.name == "engine")
        .unwrap();
    let member = MemberRoot::linked(configured, worktree.clone()).unwrap();
    member.verify_identity().unwrap();
    let cache = hashed_member_cache(&worktree, Some(fixture.cache.path())).unwrap();
    assert!(!cache.exists());

    let original_deadline = QueryDeadline::after(Duration::from_secs(30));
    let exhausted_slice = QueryDeadline::after(Duration::ZERO);
    let origin = InvocationDirectory::root();
    let (store, source, parent_read) = acquire_home_read(
        &member,
        &cache,
        Some(fixture.cache.path()),
        ("sym:SharedThing", None, &origin),
        true,
        &original_deadline,
        &exhausted_slice,
    )
    .unwrap();
    assert_eq!(store.index_root(), parent.canonicalize().unwrap().as_path());
    assert_eq!(store.root, worktree);
    assert_eq!(store.results_cache(), cache.as_path());
    assert_eq!(source.path, "lib.rs");
    assert!(source.verified);
    assert_eq!(
        parent_read,
        Some(ParentRead::Reextracted),
        "the selected parent view still reads current worktree bytes"
    );
    assert!(
        String::from_utf8_lossy(&source.bytes)
            .contains("pub struct SharedThing { pub current: bool }"),
        "the answer comes from the changed worktree source"
    );
    assert!(
        cache.join(crate::results::RESULTS_DATABASE).is_file(),
        "the parent answer stores its result in the selected cache"
    );
    assert!(!cache.join("index.sqlite3").exists());
    assert!(!cache.join("index.lock").exists());
    drop(store);

    let (daemon, client) = UnixStream::pair().unwrap();
    let cancelled_deadline =
        QueryDeadline::after(Duration::from_secs(30)).with_peer(Arc::new(daemon));
    drop(client);
    let error = match acquire_home_read(
        &member,
        &cache,
        Some(fixture.cache.path()),
        ("sym:SharedThing", None, &origin),
        true,
        &cancelled_deadline,
        &exhausted_slice,
    ) {
        Ok(_) => panic!("an original peer cancellation must fail the read"),
        Err(error) => error,
    };
    assert!(
        tp_core::coded_error::ErrorCode::TimedOut.is_code_of(&error),
        "{error:#}"
    );
    assert!(!cache.join("index.sqlite3").exists());
    assert!(!cache.join("index.lock").exists());
}

fn assert_cache_root(cache: &Path, root: &Path) {
    let marker = fs::read_to_string(cache.join(crate::system::sweep::CACHE_ROOT_MARKER)).unwrap();
    assert_eq!(marker, encode_path(&root.canonicalize().unwrap()));
}
