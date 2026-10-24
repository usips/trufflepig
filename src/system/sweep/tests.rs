use super::*;
use crate::store::Store;

fn scratch() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("trufflepig-sweep-")
        .tempdir_in(std::env::var_os("TMPDIR").expect("TMPDIR must name disk-backed scratch space"))
        .unwrap()
}

fn sweep_now(base: &Path) -> SweepReport {
    sweep(base, SystemTime::now(), SWEEP_GRACE)
}

/// Creates a published cache under `base` for `root`, returning both paths.
fn cache_for(base: &Path, root: &Path, name: &str) -> PathBuf {
    fs::create_dir_all(root).unwrap();
    fs::write(root.join("lib.rs"), "fn swept() {}\n").unwrap();
    let cache = base.join(name);
    Store::open(root, &cache).unwrap().index().unwrap();
    cache
}

fn age(path: &Path, by: Duration) {
    fs::File::open(path)
        .unwrap()
        .set_modified(SystemTime::now() - by)
        .unwrap();
}

fn age_cache(cache: &Path) {
    age(&cache.join("index.sqlite3"), 2 * SWEEP_GRACE);
    age(cache, 2 * SWEEP_GRACE);
}

#[test]
fn sweep_evicts_vanished_root_and_keeps_live_root() {
    let scratch = scratch();
    let base = scratch.path().join("base");
    let roots = scratch.path().join("roots");
    let live = cache_for(&base, &roots.join("live"), "live-cache");
    let gone = cache_for(&base, &roots.join("gone"), "gone-cache");
    let gone_root = roots.join("gone").canonicalize().unwrap();
    fs::remove_dir_all(&gone_root).unwrap();
    age_cache(&live);
    age_cache(&gone);
    let evicted = sweep_now(&base).evicted;
    assert_eq!(evicted.len(), 1, "{evicted:?}");
    assert_eq!(evicted[0].cache, gone);
    assert_eq!(evicted[0].root, gone_root);
    assert!(!evicted[0].daemon_stopped);
    assert!(!gone.exists());
    assert!(live.join("index.sqlite3").is_file());
    assert!(sweep_now(&base).evicted.is_empty());
}

#[test]
fn sweep_respects_grace_period() {
    let scratch = scratch();
    let base = scratch.path().join("base");
    let root = scratch.path().join("roots").join("gone");
    let cache = cache_for(&base, &root, "gone-cache");
    fs::remove_dir_all(&root).unwrap();
    assert!(sweep_now(&base).evicted.is_empty());
    assert!(cache.join("index.sqlite3").is_file());
    age(&cache.join("index.sqlite3"), 2 * SWEEP_GRACE);
    assert!(sweep_now(&base).evicted.is_empty());
    age(&cache, 2 * SWEEP_GRACE);
    assert_eq!(sweep_now(&base).evicted.len(), 1);
    assert!(!cache.exists());
}

#[test]
fn sweep_ignores_dirs_without_index() {
    let scratch = scratch();
    let base = scratch.path().join("base");
    let history = base.join("history");
    fs::create_dir_all(&history).unwrap();
    fs::write(history.join("history.sqlite3"), b"kept").unwrap();
    fs::write(base.join("stray-file"), b"kept").unwrap();
    age(&history, 2 * SWEEP_GRACE);
    assert!(sweep_now(&base).evicted.is_empty());
    assert!(history.join("history.sqlite3").is_file());
    assert!(
        sweep_now(&scratch.path().join("missing"))
            .evicted
            .is_empty()
    );
}

#[test]
fn sweep_evicts_marked_cache_without_index() {
    let scratch = scratch();
    let base = scratch.path().join("base");
    let root = scratch.path().join("roots").join("worktree");
    fs::create_dir_all(&root).unwrap();
    let cache = base.join("worktree-cache");
    fs::create_dir_all(&cache).unwrap();
    fs::write(cache.join("results.sqlite3"), b"results").unwrap();
    record_cache_root(&cache, &root.canonicalize().unwrap());
    age(&cache.join(CACHE_ROOT_MARKER), 2 * SWEEP_GRACE);
    age(&cache, 2 * SWEEP_GRACE);
    assert!(sweep_now(&base).evicted.is_empty());
    fs::remove_dir_all(&root).unwrap();
    let evicted = sweep_now(&base).evicted;
    assert_eq!(evicted.len(), 1, "{evicted:?}");
    assert!(!cache.exists());
}

/// Records `root` in a marker-only cache, so the root never has to exist.
fn marked_cache(base: &Path, root: &Path, name: &str) -> PathBuf {
    let cache = base.join(name);
    fs::create_dir_all(&cache).unwrap();
    record_cache_root(&cache, root);
    age(&cache.join(CACHE_ROOT_MARKER), 2 * SWEEP_GRACE);
    age(&cache, 2 * SWEEP_GRACE);
    cache
}

#[test]
fn sweep_evicts_deleted_worktree_whose_parent_is_gone() {
    let scratch = scratch();
    let base = scratch.path().join("base");
    let worktrees = scratch.path().join("repo").join(".worktrees");
    let cache = cache_for(&base, &worktrees.join("lane"), "worktree-cache");
    fs::remove_dir_all(&worktrees).unwrap();
    age_cache(&cache);
    let evicted = sweep_now(&base).evicted;
    assert_eq!(evicted.len(), 1, "{evicted:?}");
    assert!(!cache.exists());
}

#[test]
fn sweep_keeps_roots_that_may_be_on_unmounted_volumes() {
    let scratch = scratch();
    let base = scratch.path().join("base");
    // `/proc` is a mount point on another device, like an unmounted volume's parent mount.
    let mounted = marked_cache(
        &base,
        Path::new("/proc/trufflepig-unmounted-volume/repo"),
        "mounted-cache",
    );
    let blocker = scratch.path().join("not-a-directory");
    fs::write(&blocker, b"file").unwrap();
    let shadowed = marked_cache(&base, &blocker.join("repo"), "shadowed-cache");
    assert!(sweep_now(&base).evicted.is_empty());
    assert!(mounted.join(CACHE_ROOT_MARKER).is_file());
    assert!(shadowed.join(CACHE_ROOT_MARKER).is_file());
}

#[test]
fn sweep_removes_orphan_stage_only_when_writer_lease_is_free() {
    let scratch = scratch();
    let base = scratch.path().join("base");
    let cache = cache_for(&base, &scratch.path().join("roots").join("live"), "live");
    let orphan = cache.join("stage-orphan");
    let checkpoint = cache.join("stage-index-checkpoint");
    fs::create_dir_all(&orphan).unwrap();
    fs::write(orphan.join("index.sqlite3"), b"partial").unwrap();
    fs::create_dir_all(&checkpoint).unwrap();
    let lease = OpenOptions::new()
        .read(true)
        .write(true)
        .open(cache.join("index.lock"))
        .unwrap();
    lease.try_lock_exclusive().unwrap();
    let locked = sweep_now(&base);
    assert!(locked.removed_stages.is_empty(), "{locked:?}");
    assert!(orphan.join("index.sqlite3").is_file());
    drop(lease);
    let unlocked = sweep_now(&base);
    assert_eq!(unlocked.removed_stages, vec![orphan.clone()]);
    assert!(unlocked.evicted.is_empty());
    assert!(!orphan.exists());
    assert!(checkpoint.is_dir());
    assert!(cache.join("index.sqlite3").is_file());
}
