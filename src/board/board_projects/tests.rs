use super::*;
use crate::board::repo_identity::tests::GitFixture;
use crate::workspace::config::WorkspaceConfig;

mod project_fixture;
use project_fixture::ProjectFixture;

#[test]
fn project_maps_two_repos_and_a_subdir_member() {
    let first = GitFixture::new();
    first.commit("first project root");
    let second = GitFixture::new();
    second.commit("second project root");
    let subdir = second.root.join("src");
    fs::create_dir(&subdir).unwrap();
    let mut fixture = ProjectFixture::new();
    let workspace = fixture.workspace("both", "Both", &[("a", &first.root), ("b", &subdir)]);
    let stored = RepoKey::parse(&"a".repeat(40)).unwrap();
    fixture.stored_key(&second.root, "fixture-host", &stored);
    let before = fixture.board_counts();
    let projects = fixture.resolve("fixture-host");
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].project.id, workspace.id);
    assert_eq!(projects[0].project.name, "Both");
    assert_eq!(
        projects[0].project.repo_keys,
        BTreeSet::from([first.registration().repo_key, stored,])
    );
    assert!(projects[0].project.unavailable.is_empty());
    assert_eq!(fixture.board_counts(), before);
}

#[test]
fn worktree_member_maps_to_its_repo_key() {
    let git = GitFixture::new();
    git.commit("worktree repository root");
    let linked = git.directory.path().join("linked");
    git.git(&[
        "worktree",
        "add",
        "--quiet",
        "--detach",
        linked.to_str().unwrap(),
    ]);
    let mut fixture = ProjectFixture::new();
    fixture.workspace("linked", "Linked", &[("checkout", &linked)]);
    let stored = RepoKey::parse(&"b".repeat(40)).unwrap();
    fixture.stored_key(&git.root, "fixture-host", &stored);
    let projects = fixture.resolve("fixture-host");
    assert_eq!(projects[0].project.repo_keys, BTreeSet::from([stored]));
    assert!(projects[0].project.unavailable.is_empty());
}

#[test]
fn stale_registry_entry_is_unavailable() {
    let git = GitFixture::new();
    git.commit("available project root");
    let mut fixture = ProjectFixture::new();
    let valid = fixture.workspace("valid", "Valid", &[("repo", &git.root)]);
    let stale = fixture.workspace("stale", "Stale", &[("repo", &git.root)]);
    fs::remove_file(&stale.path).unwrap();
    let projects = fixture.resolve("fixture-host");
    assert_eq!(projects.len(), 2);
    assert!(
        projects
            .iter()
            .find(|project| project.project.id == valid.id)
            .unwrap()
            .project
            .unavailable
            .is_empty()
    );
    let stale_project = &projects
        .iter()
        .find(|project| project.project.id == stale.id)
        .unwrap()
        .project;
    assert!(stale_project.repo_keys.is_empty());
    assert_eq!(stale_project.unavailable.len(), 1);
    assert_eq!(stale_project.unavailable[0].root, stale.path);
    assert!(!stale_project.unavailable[0].available);
    assert!(!fixture.config.db_path.exists());
}

#[test]
fn absent_board_resolves_without_creating_storage() {
    let git = GitFixture::new();
    git.commit("unregistered repository root");
    let mut fixture = ProjectFixture::new();
    fixture.workspace("fresh", "Fresh", &[("repo", &git.root)]);
    let projects = fixture.resolve("fixture-host");
    assert_eq!(
        projects[0].project.repo_keys,
        BTreeSet::from([git.registration().repo_key])
    );
    assert!(!fixture.config.db_path.exists());
    assert!(
        !fixture
            .config
            .db_path
            .with_extension("sqlite3-wal")
            .exists()
    );
    assert!(
        !fixture
            .config
            .db_path
            .with_extension("sqlite3-shm")
            .exists()
    );
}

#[test]
fn existing_corrupt_board_is_not_treated_as_absent() {
    let git = GitFixture::new();
    git.commit("root with corrupt storage");
    let mut fixture = ProjectFixture::new();
    fixture.workspace("broken", "Broken", &[("repo", &git.root)]);
    let before = b"existing invalid SQLite database";
    fs::write(&fixture.config.db_path, before).unwrap();
    let error = fixture
        .resolver
        .resolve(&fixture.config, "fixture-host", QueryDeadline::start())
        .unwrap_err();
    assert_eq!(
        error.code,
        crate::board::board_protocol::BoardErrorCode::BoardUnavailable
    );
    assert_eq!(fs::read(&fixture.config.db_path).unwrap(), before);
}

#[test]
fn existing_unreadable_board_is_not_treated_as_absent() {
    use std::os::unix::fs::PermissionsExt;
    let git = GitFixture::new();
    git.commit("root with unreadable storage");
    let mut fixture = ProjectFixture::new();
    fixture.workspace("unreadable", "Unreadable", &[("repo", &git.root)]);
    fixture.stored_key(
        &git.root,
        "fixture-host",
        &RepoKey::parse(&"c".repeat(40)).unwrap(),
    );
    fs::set_permissions(&fixture.config.db_path, fs::Permissions::from_mode(0o000)).unwrap();
    let result = fixture
        .resolver
        .resolve(&fixture.config, "fixture-host", QueryDeadline::start());
    fs::set_permissions(&fixture.config.db_path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        result.is_err(),
        "existing unreadable storage must not silently fall back"
    );
}

#[test]
fn dangling_board_symlink_is_not_treated_as_absent() {
    let mut fixture = ProjectFixture::new();
    let target = fixture.directory.path().join("missing.sqlite3");
    std::os::unix::fs::symlink(&target, &fixture.config.db_path).unwrap();
    let error = fixture
        .resolver
        .resolve(&fixture.config, "fixture-host", QueryDeadline::start())
        .unwrap_err();
    assert!(error.message.contains("symbolic link"));
    assert!(!target.exists());
    assert!(
        fs::symlink_metadata(&fixture.config.db_path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn invalid_config_and_missing_or_non_git_members_remain_visible() {
    let mut fixture = ProjectFixture::new();
    let plain = fixture.directory.path().join("plain");
    fs::create_dir(&plain).unwrap();
    let missing = fixture.directory.path().join("missing");
    fixture.workspace(
        "members",
        "Members",
        &[("missing", &missing), ("plain", &plain)],
    );
    let bad = fixture.workspace("bad", "Bad", &[("plain", &plain)]);
    fs::write(&bad.path, "invalid = [").unwrap();
    let projects = fixture.resolve("fixture-host");
    assert_eq!(projects.len(), 2);
    let members = &projects
        .iter()
        .find(|project| project.raw_name == "Members")
        .unwrap()
        .project;
    assert!(members.repo_keys.is_empty());
    assert_eq!(
        members
            .unavailable
            .iter()
            .map(|member| member.name.as_str())
            .collect::<Vec<_>>(),
        ["missing", "plain"]
    );
    assert!(members.unavailable.iter().all(|member| !member.available));
    let invalid = &projects
        .iter()
        .find(|project| project.project.id == bad.id)
        .unwrap()
        .project;
    assert_eq!(invalid.unavailable[0].root, bad.path);
}

#[test]
fn colliding_display_names_retain_raw_names() {
    let git = GitFixture::new();
    git.commit("shared workspace root");
    let mut fixture = ProjectFixture::new();
    fixture.workspace("left", "Shared", &[("repo", &git.root)]);
    fixture.workspace("right", "Shared", &[("repo", &git.root)]);
    let projects = fixture.resolve("fixture-host");
    assert_eq!(projects.len(), 2);
    for project in &projects {
        assert_eq!(project.raw_name, "Shared");
        assert_eq!(
            project.project.name,
            format!("Shared·{}", &project.project.id[..6])
        );
    }
    assert_ne!(projects[0].project.name, projects[1].project.name);
}

#[test]
fn stored_identity_lookup_preserves_lossy_common_directory_encoding() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let git = GitFixture::new();
    git.commit("non UTF-8 common directory root");
    let raw_name = b"common-\xff.git";
    fs::rename(
        git.root.join(".git"),
        git.root.join(OsString::from_vec(raw_name.to_vec())),
    )
    .unwrap();
    let mut pointer = b"gitdir: ".to_vec();
    pointer.extend_from_slice(raw_name);
    pointer.push(b'\n');
    fs::write(git.root.join(".git"), pointer).unwrap();
    let mut fixture = ProjectFixture::new();
    fixture.workspace("lossy", "Lossy", &[("repo", &git.root)]);
    let stored = RepoKey::parse(&"d".repeat(40)).unwrap();
    fixture.stored_key(&git.root, "fixture-host", &stored);
    let projects = fixture.resolve("fixture-host");
    assert_eq!(projects[0].project.repo_keys, BTreeSet::from([stored]));
    assert!(projects[0].project.unavailable.is_empty());
}

mod project_cache_tests;
