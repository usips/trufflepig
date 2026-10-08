use super::*;

#[test]
fn registry_and_config_edits_invalidate_the_project_cache() {
    let git = GitFixture::new();
    git.commit("cache configuration root");
    let mut fixture = ProjectFixture::new();
    let first = fixture.workspace("first", "First", &[("repo", &git.root)]);
    assert_eq!(fixture.resolve("fixture-host")[0].raw_name, "First");
    let body = fs::read_to_string(&first.path)
        .unwrap()
        .replace("First", "Renamed-first");
    fs::write(&first.path, body).unwrap();
    assert_eq!(fixture.resolve("fixture-host")[0].raw_name, "Renamed-first");
    fixture.workspace("second", "Second", &[("repo", &git.root)]);
    assert_eq!(fixture.resolve("fixture-host").len(), 2);
    fs::remove_file(&first.path).unwrap();
    let projects = fixture.resolve("fixture-host");
    let stale = &projects
        .iter()
        .find(|project| project.project.id == first.id)
        .unwrap()
        .project;
    assert!(stale.repo_keys.is_empty());
    assert_eq!(stale.unavailable[0].root, first.path);
}

#[test]
fn host_and_database_changes_do_not_reuse_cached_bindings() {
    let git = GitFixture::new();
    git.commit("host-specific identity root");
    let mut fixture = ProjectFixture::new();
    fixture.workspace("hosts", "Hosts", &[("repo", &git.root)]);
    let first = RepoKey::parse(&"1".repeat(40)).unwrap();
    let second = RepoKey::parse(&"2".repeat(40)).unwrap();
    fixture.stored_key(&git.root, "first-host", &first);
    fixture.stored_key(&git.root, "second-host", &second);
    assert_eq!(
        fixture.resolve("first-host")[0].project.repo_keys,
        BTreeSet::from([first])
    );
    assert_eq!(
        fixture.resolve("second-host")[0].project.repo_keys,
        BTreeSet::from([second])
    );
    fixture.config.db_path = fixture.directory.path().join("another.sqlite3");
    assert_eq!(
        fixture.resolve("second-host")[0].project.repo_keys,
        BTreeSet::from([git.registration().repo_key])
    );
    assert!(!fixture.config.db_path.exists());
}

#[test]
fn newly_created_board_replaces_cached_fallback_identity() {
    let git = GitFixture::new();
    git.commit("absent to present database root");
    let mut fixture = ProjectFixture::new();
    fixture.workspace("appeared", "Appeared", &[("repo", &git.root)]);
    assert_eq!(
        fixture.resolve("fixture-host")[0].project.repo_keys,
        BTreeSet::from([git.registration().repo_key])
    );
    let stored = RepoKey::parse(&"3".repeat(40)).unwrap();
    fixture.stored_key(&git.root, "fixture-host", &stored);
    assert_eq!(
        fixture.resolve("fixture-host")[0].project.repo_keys,
        BTreeSet::from([stored])
    );
}

#[test]
fn project_cache_expires_and_rechecks_missing_members() {
    let git = GitFixture::new();
    git.commit("expiring member root");
    let mut fixture = ProjectFixture::new();
    fixture.workspace("expiry", "Expiry", &[("repo", &git.root)]);
    assert!(
        !fixture.resolve("fixture-host")[0]
            .project
            .repo_keys
            .is_empty()
    );
    fs::remove_dir_all(&git.root).unwrap();
    assert!(
        !fixture.resolve("fixture-host")[0]
            .project
            .repo_keys
            .is_empty()
    );
    fixture.resolver.cached.as_mut().unwrap().expires = Instant::now();
    let projects = fixture.resolve("fixture-host");
    assert!(projects[0].project.repo_keys.is_empty());
    assert_eq!(projects[0].project.unavailable[0].name, "repo");
}

#[test]
fn origin_override_changes_invalidate_cached_fallback_identity() {
    let git = GitFixture::new();
    git.commit("origin override root");
    git.git(&[
        "remote",
        "add",
        "origin",
        "https://example.test/project.git",
    ]);
    let mut fixture = ProjectFixture::new();
    fixture.workspace("override", "Override", &[("repo", &git.root)]);
    assert_eq!(
        fixture.resolve("fixture-host")[0].project.repo_keys,
        BTreeSet::from([git.registration().repo_key])
    );
    let overridden = RepoKey::parse(&"4".repeat(40)).unwrap();
    fixture.config.repos.insert(
        "https://example.test/project.git".into(),
        overridden.clone(),
    );
    assert_eq!(
        fixture.resolve("fixture-host")[0].project.repo_keys,
        BTreeSet::from([overridden])
    );
    assert!(!fixture.config.db_path.exists());
}

#[test]
fn missing_registry_is_empty_and_malformed_registry_is_an_error() {
    let mut fixture = ProjectFixture::new();
    assert!(fixture.resolve("fixture-host").is_empty());
    fs::write(&fixture.registry, "workspaces = [").unwrap();
    assert!(
        fixture
            .resolver
            .resolve(&fixture.config, "fixture-host", QueryDeadline::start())
            .is_err()
    );
    assert!(!fixture.config.db_path.exists());
}

#[test]
fn expired_project_resolution_does_not_create_storage() {
    let mut fixture = ProjectFixture::new();
    let error = fixture
        .resolver
        .resolve(
            &fixture.config,
            "fixture-host",
            QueryDeadline::after(Duration::ZERO),
        )
        .unwrap_err();
    assert!(error.message.contains("timed_out"));
    assert!(!fixture.config.db_path.exists());
}
