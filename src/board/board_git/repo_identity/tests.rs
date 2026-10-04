use super::*;
use std::cell::Cell;
use std::process::Command;

pub(crate) struct GitFixture {
    pub directory: tempfile::TempDir,
    pub root: PathBuf,
    clock: Cell<i64>,
}

impl GitFixture {
    pub fn new() -> Self {
        let directory = crate::board::board_test_support::scratch("board-fixture-");
        let root = directory.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let fixture = Self {
            directory,
            root,
            clock: Cell::new(1_700_000_000),
        };
        fixture.git(&["init", "--quiet", "--initial-branch=main"]);
        fixture
    }

    pub fn git(&self, args: &[&str]) -> String {
        self.git_in(&self.root, args)
    }

    pub fn git_in(&self, directory: &Path, args: &[&str]) -> String {
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        let stamp = format!("{} +0000", self.clock.get());
        command
            .current_dir(directory)
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.test",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_DATE", &stamp)
            .env("GIT_COMMITTER_DATE", stamp)
            .args(args);
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "Git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    pub fn commit(&self, message: &str) -> GitOid {
        self.clock.set(self.clock.get() + 1);
        self.git(&["commit", "--allow-empty", "--quiet", "-m", message]);
        GitOid::parse(self.git(&["rev-parse", "HEAD"]).trim()).unwrap()
    }

    pub fn registration(&self) -> RepoRegistration {
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
    if crate::board::board_test_support::git_version() < Some((2, 46)) {
        eprintln!(concat!(
            "skipping reftable_repository_registration_resolves_symbolic_head: ",
            "requires Git >= 2.46 for git refs migrate"
        ));
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
