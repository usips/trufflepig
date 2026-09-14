use super::*;

#[test]
fn leases_expire_and_can_be_renewed_without_duplicate_roots() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("root");
    let cache = directory.path().join("history");
    fs::create_dir(&root)?;
    register(&root, &cache)?;
    register(&root, &cache)?;
    assert_eq!(
        active_roots(&cache, clock_ms()?)?,
        vec![root.canonicalize()?]
    );
    assert!(active_roots(&cache, clock_ms()? + 16_000)?.is_empty());
    assert_eq!(fs::read_dir(cache.join(REGISTRATIONS))?.count(), 0);
    register(&root, &cache)?;
    assert_eq!(active_roots(&cache, clock_ms()?)?.len(), 1);
    Ok(())
}

#[test]
fn worktrees_have_independent_leases() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cache = directory.path().join("history");
    let left = directory.path().join("left");
    let right = directory.path().join("right");
    fs::create_dir(&left)?;
    fs::create_dir(&right)?;
    register(&left, &cache)?;
    register(&right, &cache)?;
    let left_key = blake3::hash(left.canonicalize()?.as_os_str().as_bytes())
        .to_hex()
        .to_string();
    let stale = Registration {
        root: left,
        heartbeat_ms: clock_ms()? - 20_000,
    };
    fs::write(
        cache.join(REGISTRATIONS).join(left_key),
        serde_json::to_vec(&stale)?,
    )?;
    assert_eq!(
        active_roots(&cache, clock_ms()?)?,
        vec![right.canonicalize()?]
    );
    Ok(())
}

#[test]
fn worker_lock_excludes_duplicates_and_releases_on_exit() -> Result<()> {
    let directory = tempfile::tempdir()?;
    assert!(!is_running(directory.path())?);
    let first = worker_lock(directory.path())?;
    first.try_lock_exclusive()?;
    assert!(is_running(directory.path())?);
    let second = worker_lock(directory.path())?;
    assert_eq!(
        second.try_lock_exclusive().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    serve(directory.path())?;
    drop(first);
    assert!(!is_running(directory.path())?);
    second.try_lock_exclusive()?;
    drop(second);
    serve(directory.path())?;
    let recovered = worker_lock(directory.path())?;
    recovered.try_lock_exclusive()?;
    Ok(())
}

#[test]
fn worker_exit_unlocks_despite_an_inherited_file_description() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let owner = worker_lock(directory.path())?;
    owner.try_lock_exclusive()?;
    // A dup retains the same open-file description as a fork before exec.
    let inherited = owner.try_clone()?;
    drop(owner);
    assert!(!is_running(directory.path())?);
    drop(inherited);
    Ok(())
}

#[test]
fn oversized_or_malformed_leases_do_not_keep_worker_alive() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cache = directory.path();
    private_directory(&cache.join(REGISTRATIONS))?;
    fs::write(cache.join(REGISTRATIONS).join("malformed"), b"{broken")?;
    let oversized = File::create(cache.join(REGISTRATIONS).join("oversized"))?;
    oversized.set_len(MAX_REGISTRATION_BYTES + 1)?;
    serve(cache)?;
    assert_eq!(fs::read_dir(cache.join(REGISTRATIONS))?.count(), 0);
    Ok(())
}

#[test]
fn registration_cache_is_private() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cache = directory.path().join("cache");
    register(directory.path(), &cache)?;
    assert_eq!(fs::metadata(&cache)?.permissions().mode() & 0o777, 0o700);
    let registration = fs::read_dir(cache.join(REGISTRATIONS))?
        .next()
        .unwrap()?
        .path();
    assert_eq!(
        fs::metadata(registration)?.permissions().mode() & 0o777,
        0o600
    );
    Ok(())
}

#[test]
fn running_worker_stops_after_its_last_lease_expires() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cache = directory.path().join("cache");
    register(directory.path(), &cache)?;
    let (finished, receive) = std::sync::mpsc::channel();
    let worker_cache = cache.clone();
    let thread = std::thread::spawn(move || {
        let _ = finished.send(serve(&worker_cache));
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let lock = worker_lock(&cache)?;
        if lock.try_lock_exclusive().is_err() {
            break;
        }
        drop(lock);
        assert!(
            std::time::Instant::now() < deadline,
            "worker did not acquire ownership"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    {
        let _guard = registration_lock(&cache)?;
        let path = fs::read_dir(cache.join(REGISTRATIONS))?
            .next()
            .unwrap()?
            .path();
        let stale = Registration {
            root: directory.path().to_owned(),
            heartbeat_ms: 0,
        };
        fs::write(path, serde_json::to_vec(&stale)?)?;
    }
    receive.recv_timeout(Duration::from_secs(5))??;
    thread.join().unwrap();
    assert!(active_roots(&cache, clock_ms()?)?.is_empty());
    Ok(())
}

#[test]
fn cache_identity_shares_worktrees_and_isolates_clones_and_overrides() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let repository = directory.path().join("repository");
    let worktree = directory.path().join("worktree");
    let clone = directory.path().join("clone");
    fs::create_dir(&repository)?;
    let git = |root: &Path, arguments: &[&std::ffi::OsStr]| -> Result<()> {
        let output = Command::new("git")
            .current_dir(root)
            .args([
                "-c",
                "user.name=Worker Test",
                "-c",
                "user.email=worker@example.invalid",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgSign=false",
            ])
            .args(arguments)
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "fixture Git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    };
    git(&repository, &["init".as_ref(), "-q".as_ref()])?;
    git(
        &repository,
        &[
            "commit".as_ref(),
            "--allow-empty".as_ref(),
            "-qm".as_ref(),
            "root".as_ref(),
        ],
    )?;
    git(
        &repository,
        &[
            "worktree".as_ref(),
            "add".as_ref(),
            "--detach".as_ref(),
            worktree.as_os_str(),
        ],
    )?;
    git(
        directory.path(),
        &[
            "clone".as_ref(),
            "-q".as_ref(),
            repository.as_os_str(),
            clone.as_os_str(),
        ],
    )?;
    let shared = directory.path().join("shared");
    let live = directory.path().join("live");
    let canonical = resolve_cache(&repository, None, Some(&shared))?;
    assert_eq!(canonical, resolve_cache(&worktree, None, Some(&shared))?);
    assert_ne!(canonical, resolve_cache(&clone, None, Some(&shared))?);
    assert_eq!(
        canonical,
        resolve_cache(&repository, Some(&live), Some(&shared))?
    );
    assert!(resolve_cache(&repository, Some(&live), None)?.starts_with(live.join("history")));
    Ok(())
}
