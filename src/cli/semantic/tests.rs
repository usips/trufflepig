use super::*;

#[test]
fn preparation_wait_terminates_when_root_daemon_dies() -> Result<()> {
    let root = tempfile::tempdir()?;
    let cache = tempfile::tempdir()?;
    std::fs::write(root.path().join("source.rs"), "fn example() {}")?;
    let mut store = crate::store::Store::open(root.path(), cache.path())?;
    store.index()?;
    let receipt = preparation::schedule(root.path(), cache.path())?;
    let result = wait_until_terminal(root.path(), cache.path(), receipt.captured_generation)?;
    assert!(result.is_terminal());
    assert_eq!(
        result.status.error.as_deref(),
        Some("preparation_daemon_stopped")
    );
    Ok(())
}

#[test]
fn preparation_wait_checks_actual_lock_ownership() -> Result<()> {
    use fs2::FileExt;
    let cache = tempfile::tempdir()?;
    let lock = std::fs::File::create(cache.path().join("daemon.lock"))?;
    lock.try_lock_exclusive()?;
    assert!(root_daemon_running(cache.path())?);
    FileExt::unlock(&lock)?;
    assert!(!root_daemon_running(cache.path())?);
    Ok(())
}
