//! A throwaway Git repository with a deterministic author and clock.
use crate::identity::GitOid;
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct GitFixture {
    pub directory: tempfile::TempDir,
    pub root: PathBuf,
    clock: Cell<i64>,
}

impl GitFixture {
    pub fn new() -> Self {
        let directory = super::scratch("git-fixture-");
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
}
