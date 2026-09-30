//! Three-member workspace fixture (`engine`, `pack`, `upstream`, each defining
//! `SharedThing`) run through the CLI with `--no-daemon` and an explicit cache.
use super::{WorkspaceConfig, member_root::MemberRoot};
use crate::cli;
use anyhow::Result;
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

pub(in crate::workspace) fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

pub(in crate::workspace) struct Fixture {
    pub root: tempfile::TempDir,
    pub cache: tempfile::TempDir,
    pub config: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        for member in ["engine", "pack", "upstream"] {
            fs::create_dir(root.path().join(member)).unwrap();
            fs::write(
                root.path().join(member).join("lib.rs"),
                format!("pub struct SharedThing {{ pub {member}_marker: u8 }}\n"),
            )
            .unwrap();
        }
        let config = root.path().join("trufflepig.workspace.toml");
        let fixture = Self {
            root,
            cache,
            config,
        };
        fixture.configure(&["engine", "pack", "upstream"]);
        fixture
    }

    /// Members become Git repositories with one commit each, so linked
    /// worktrees can be created from them.
    pub fn git_backed() -> Self {
        let fixture = Self::new();
        for member in ["engine", "pack", "upstream"] {
            let root = fixture.root.path().join(member);
            git(&root, &["init", "-q", "-b", "main"]);
            git(&root, &["add", "."]);
            git(&root, &["commit", "-q", "-m", "init"]);
        }
        fixture
    }

    /// Adds a detached linked worktree of `member` at `path`.
    pub fn worktree(&self, member: &str, path: &Path) -> PathBuf {
        git(
            &self.root.path().join(member),
            &["worktree", "add", "--detach", path.to_str().unwrap()],
        );
        path.canonicalize().unwrap()
    }

    pub fn run_at(&self, root: &Path, words: &[&str]) -> Result<String> {
        let mut args = vec![
            "--workspace".into(),
            self.config.display().to_string(),
            "--root".into(),
            root.display().to_string(),
            "--cache".into(),
            self.cache.path().display().to_string(),
            "--no-daemon".into(),
            "--diagnostics".into(),
            "off".into(),
        ];
        args.extend(words.iter().map(|word| (*word).to_owned()));
        cli::run(&args)
    }

    pub fn configure(&self, members: &[&str]) {
        let mut text = "[workspace]\nname = 'test-workspace'\n".to_owned();
        for member in members {
            text.push_str(&format!("[members.{member}]\npath = '{member}'\n"));
        }
        fs::write(&self.config, text).unwrap();
    }

    pub fn run(&self, home: &str, words: &[&str]) -> Result<String> {
        let mut args = vec![
            "--workspace".into(),
            self.config.display().to_string(),
            "--root".into(),
            self.root.path().join(home).display().to_string(),
            "--cache".into(),
            self.cache.path().display().to_string(),
            "--no-daemon".into(),
            "--diagnostics".into(),
            "off".into(),
        ];
        args.extend(words.iter().map(|word| (*word).to_owned()));
        cli::run(&args)
    }

    pub fn json(&self, home: &str, words: &[&str]) -> Value {
        serde_json::from_str(&self.run(home, words).unwrap()).unwrap()
    }

    pub fn member_cache(&self, name: &str) -> PathBuf {
        let config = WorkspaceConfig::load(&self.config).unwrap();
        let member = config
            .members
            .iter()
            .find(|member| member.name == name)
            .unwrap();
        super::member_cache(&MemberRoot::configured(member), Some(self.cache.path())).unwrap()
    }

    pub fn handle(&self, member: &str) -> String {
        self.json(
            "engine",
            &["search", "sym:SharedThing", &format!("in:{member}")],
        )["hits"][0]["handle"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}
