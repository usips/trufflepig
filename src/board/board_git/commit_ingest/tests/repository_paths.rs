use super::*;

#[test]
fn branch_and_main_and_linked_detached_commits_are_all_scanned() {
    let fixture = GitFixture::new();
    let root = fixture.commit("root");
    fixture.git(&["checkout", "--quiet", "-b", "other"]);
    let branch = fixture.commit(&linked_message("noncurrent branch"));
    fixture.git(&["checkout", "--quiet", "--detach", root.as_str()]);
    let main_detached = fixture.commit(&linked_message("main detached"));
    let worktree = fixture.directory.path().join("linked");
    fixture.git(&[
        "worktree",
        "add",
        "--quiet",
        "--detach",
        worktree.to_str().unwrap(),
        root.as_str(),
    ]);
    fixture.git_in(
        &worktree,
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            &linked_message("linked detached"),
        ],
    );
    let linked_detached =
        GitOid::parse(fixture.git_in(&worktree, &["rev-parse", "HEAD"]).trim()).unwrap();
    let target = target(&fixture);
    let mut backend = TestBackend::default();
    let mut ingestor = RepoIngestor::default();
    let report = ingestor
        .ingest(
            &mut backend,
            &actor(),
            &[target.clone()],
            Duration::from_secs(5),
        )
        .unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let actual: BTreeSet<_> = backend.linked.iter().map(|commit| commit.oid).collect();
    assert_eq!(
        actual,
        BTreeSet::from([branch, main_detached, linked_detached])
    );
    let report = ingestor
        .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
        .unwrap();
    assert_eq!(report.skipped, 1);
    assert_eq!(backend.requests, 1);
}

#[test]
fn missing_common_directory_is_removed_only_after_grace() {
    let fixture = GitFixture::new();
    fixture.commit(&linked_message("linked root"));
    let target = target(&fixture);
    let mut backend = TestBackend::default();
    let mut ingestor = RepoIngestor::default();
    std::fs::remove_dir_all(&target.registration.common_dir).unwrap();
    let first = ingestor
        .ingest(
            &mut backend,
            &actor(),
            &[target.clone()],
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(first.skipped, 1);
    assert_eq!(backend.forgotten, 0);
    let key = (
        target.registration.repo_key.clone(),
        target.registration.common_dir.clone(),
    );
    ingestor
        .missing
        .insert(key, Instant::now() - MISSING_REPOSITORY_GRACE);
    let second = ingestor
        .ingest(&mut backend, &actor(), &[target], Duration::from_secs(5))
        .unwrap();
    assert_eq!(second.skipped, 1);
    assert_eq!(backend.forgotten, 1);
    assert!(ingestor.missing.is_empty());
}

#[test]
fn failed_missing_path_cleanup_does_not_block_healthy_repositories() {
    let missing = GitFixture::new();
    missing.commit(&linked_message("missing root"));
    let missing_target = target(&missing);
    std::fs::remove_dir_all(&missing_target.registration.common_dir).unwrap();
    let healthy = GitFixture::new();
    healthy.commit(&linked_message("healthy root"));
    let mut backend = TestBackend {
        fail_forget: true,
        ..TestBackend::default()
    };
    let mut ingestor = RepoIngestor::default();
    let key = (
        missing_target.registration.repo_key.clone(),
        missing_target.registration.common_dir.clone(),
    );
    ingestor
        .missing
        .insert(key.clone(), Instant::now() - MISSING_REPOSITORY_GRACE);
    let report = ingestor
        .ingest(
            &mut backend,
            &actor(),
            &[missing_target, target(&healthy)],
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(report.errors.len(), 1);
    assert_eq!(report.completed.len(), 1);
    assert_eq!(backend.linked.len(), 1);
    assert!(ingestor.missing.contains_key(&key));
}

#[test]
fn stray_linked_worktree_files_do_not_abort_scans() {
    let fixture = GitFixture::new();
    fixture.commit(&linked_message("linked root"));
    std::fs::create_dir_all(fixture.root.join(".git/worktrees")).unwrap();
    std::fs::write(fixture.root.join(".git/worktrees/stray"), "unrelated").unwrap();
    std::fs::create_dir(fixture.root.join(".git/worktrees/empty-stray")).unwrap();
    let mut backend = TestBackend::default();
    let report = RepoIngestor::default()
        .ingest(
            &mut backend,
            &actor(),
            &[target(&fixture)],
            Duration::from_secs(5),
        )
        .unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.completed.len(), 1);
    assert_eq!(backend.linked.len(), 1);
}
