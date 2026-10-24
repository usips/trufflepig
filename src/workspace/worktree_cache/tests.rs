use super::super::{
    config::WorkspaceConfig, hashed_member_cache, member_cache_until, member_root::MemberRoot,
};
use crate::{
    daemon::deadline::QueryDeadline, system::sweep::CACHE_ROOT_MARKER,
    workspace::test_fixture::Fixture,
};
use std::{os::unix::net::UnixStream, path::PathBuf, sync::Arc, time::Duration};

#[test]
fn expired_read_deadline_creates_no_worktree_cache_side_effects() {
    let (_fixture, _elsewhere, member, explicit_cache, cache) = linked_worktree_fixture();
    let error = member_cache_until(
        &member,
        Some(&explicit_cache),
        &QueryDeadline::after(Duration::ZERO),
    )
    .unwrap_err();

    assert!(
        crate::coded_error::ErrorCode::TimedOut.is_code_of(&error),
        "{error:#}"
    );
    assert_no_cache_side_effects(&cache);
}

#[test]
fn cancelled_read_deadline_creates_no_worktree_cache_side_effects() {
    let (_fixture, _elsewhere, member, explicit_cache, cache) = linked_worktree_fixture();
    let (peer, client) = UnixStream::pair().unwrap();
    let deadline = QueryDeadline::start().with_peer(Arc::new(peer));
    drop(client);

    let error = member_cache_until(&member, Some(&explicit_cache), &deadline).unwrap_err();

    assert!(
        crate::coded_error::ErrorCode::TimedOut.is_code_of(&error),
        "{error:#}"
    );
    assert_no_cache_side_effects(&cache);
}

fn linked_worktree_fixture() -> (Fixture, tempfile::TempDir, MemberRoot, PathBuf, PathBuf) {
    let fixture = Fixture::git_backed();
    let elsewhere = tempfile::tempdir().unwrap();
    let root = fixture.worktree("engine", &elsewhere.path().join("wt-deadline"));
    let config = WorkspaceConfig::load(&fixture.config).unwrap();
    let engine = config
        .members
        .iter()
        .find(|member| member.name == "engine")
        .unwrap();
    let member = MemberRoot::linked(engine, root.clone()).unwrap();
    let explicit_cache = fixture.cache.path().to_path_buf();
    let cache = hashed_member_cache(&root, Some(&explicit_cache)).unwrap();
    (fixture, elsewhere, member, explicit_cache, cache)
}

fn assert_no_cache_side_effects(cache: &std::path::Path) {
    assert!(!cache.exists(), "expired read created {cache:?}");
    assert!(!cache.join(CACHE_ROOT_MARKER).exists());
    assert!(!cache.join("index.sqlite3").exists());
    assert!(!cache.join("seed-outcome.json").exists());
}
