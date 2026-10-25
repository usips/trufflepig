use super::*;
use tp_core::test_fixtures::GitFixture;

/// Registers a fixture repository as the board's fixture host would.
pub(crate) trait FixtureRegistration {
    fn registration(&self) -> RepoRegistration;
}

impl FixtureRegistration for GitFixture {
    fn registration(&self) -> RepoRegistration {
        register_repository(&self.root, "fixture-host", None, Duration::from_secs(5))
            .unwrap()
            .unwrap()
    }
}

#[test]
fn registration_is_portable_across_clones_and_linked_worktrees() {
    let fixture = GitFixture::new();
    fixture.commit("root");
    fixture.git(&[
        "remote",
        "add",
        "origin",
        "https://example.test/repository.git",
    ]);
    let registration = fixture.registration();
    let clone = fixture.directory.path().join("clone");
    fixture.git(&[
        "clone",
        "--quiet",
        fixture.root.to_str().unwrap(),
        clone.to_str().unwrap(),
    ]);
    let clone_registration =
        register_repository(&clone, "another-host", None, Duration::from_secs(5))
            .unwrap()
            .unwrap();
    assert_eq!(clone_registration.repo_key, registration.repo_key);
    let linked = fixture.directory.path().join("linked");
    fixture.git(&[
        "worktree",
        "add",
        "--quiet",
        "--detach",
        linked.to_str().unwrap(),
    ]);
    let linked_registration =
        register_repository(&linked, "fixture-host", None, Duration::from_secs(5))
            .unwrap()
            .unwrap();
    assert_eq!(linked_registration.common_dir, registration.common_dir);
    assert_eq!(linked_registration.repo_key, registration.repo_key);
    assert_eq!(
        registration.origin_label.as_deref(),
        Some("https://example.test/repository.git")
    );
}

#[test]
fn origin_label_removes_url_credentials() {
    let fixture = GitFixture::new();
    fixture.commit("root");
    fixture.git(&[
        "remote",
        "add",
        "origin",
        "https://user:secret@example.test/path@revision.git",
    ]);
    assert_eq!(
        fixture.registration().origin_label.as_deref(),
        Some("https://example.test/path@revision.git")
    );
}

#[test]
fn reftable_repository_registration_resolves_symbolic_head() {
    if !tp_core::test_fixtures::git_version::at_least(
        2,
        46,
        "reftable_repository_registration_resolves_symbolic_head",
    ) {
        return;
    }
    let fixture = GitFixture::new();
    fixture.commit("root");
    fixture.git(&["refs", "migrate", "--ref-format=reftable"]);
    assert!(
        register_repository(&fixture.root, "fixture-host", None, Duration::from_secs(5))
            .unwrap()
            .is_some()
    );
}

#[test]
fn notes_and_stash_refs_do_not_change_repository_identity() {
    let fixture = GitFixture::new();
    fixture.commit("root");
    let initial = fixture.registration();
    fixture.git(&["notes", "add", "-m", "review note", "HEAD"]);
    std::fs::write(fixture.root.join("tracked.txt"), "draft").unwrap();
    fixture.git(&["add", "tracked.txt"]);
    fixture.git(&["stash", "--quiet"]);
    let with_extras = fixture.registration();
    assert_eq!(with_extras.repo_key, initial.repo_key);
    assert_eq!(with_extras.root_commits, initial.root_commits);
}

#[test]
fn nonrepository_and_unborn_repository_have_no_portable_identity() {
    let fixture = GitFixture::new();
    assert!(
        register_repository(&fixture.root, "host", None, Duration::from_secs(5))
            .unwrap()
            .is_none()
    );
    assert!(
        register_repository(Path::new("/"), "host", None, Duration::from_secs(5))
            .unwrap()
            .is_none()
    );
    fixture.commit("root");
    assert!(fixture.registration().origin_label.is_none());
}

#[test]
fn working_roots_and_detached_heads_come_from_git_exit_codes() {
    let wait = Duration::from_secs(5);
    let fixture = GitFixture::new();
    let first = fixture.commit("root");
    let nested = fixture.root.join("nested");
    std::fs::create_dir(&nested).unwrap();
    let root = fixture.root.canonicalize().unwrap();
    assert_eq!(repository_root(&nested, wait).unwrap(), Some(root));
    assert_eq!(
        repository_root(&fixture.root.join(".git"), wait).unwrap(),
        None
    );
    assert_eq!(repository_root(Path::new("/"), wait).unwrap(), None);
    let bare = fixture.directory.path().join("bare.git");
    fixture.git(&["clone", "--quiet", "--bare", ".", bare.to_str().unwrap()]);
    assert_eq!(repository_root(&bare, wait).unwrap(), None);
    let missing = fixture.directory.path().join("missing");
    assert!(repository_root(&missing, wait).is_err());

    let git_dir = fixture.root.join(".git");
    let deadline = Instant::now() + wait;
    assert_eq!(detached_head(&git_dir, deadline).unwrap(), None);
    fixture.git(&["checkout", "--quiet", "--detach", "HEAD"]);
    assert_eq!(detached_head(&git_dir, deadline).unwrap(), Some(first));
    assert_eq!(fixture.registration().root_commits, vec![first]);
}
