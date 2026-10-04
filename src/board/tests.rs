//! Acceptance fixtures exercise the edge and durable backend together.

mod client_tests;

mod cursor_acceptance;
mod git_acceptance;
mod transport_acceptance;

use super::board_actor::{BoardActor, HarnessLabel};
use super::board_backend::BoardHost;
use super::board_config::BoardConfig;
use super::board_grammar;
use crate::cli::{self, Arguments};
use crate::daemon::deadline::QueryDeadline;
use crate::diagnostics::RequestContext;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

struct EdgeFixture {
    scratch: TempDir,
    root: PathBuf,
    config: BoardConfig,
    host: BoardHost,
}

impl EdgeFixture {
    fn new() -> Self {
        let scratch = crate::board::board_test_support::scratch("acceptance-");
        let root = scratch.path().join("repository");
        std::fs::create_dir(&root).unwrap();
        // An unborn fixture prevents Git discovery from reaching the real checkout.
        git(&root, &["init", "--initial-branch=main"]);
        let mut config = BoardConfig::for_database(scratch.path().join("data/board.sqlite3"));
        config.user = "acceptance".into();
        config.host = "edge-host".into();
        let host = BoardHost::with_config(config.clone());
        Self {
            scratch,
            root,
            config,
            host,
        }
    }

    fn options(&self, words: &[&str]) -> Arguments {
        self.options_at(&self.root, words)
    }

    fn options_at(&self, root: &Path, words: &[&str]) -> Arguments {
        let mut args = vec![
            "--root".into(),
            root.to_str().unwrap().into(),
            "--budget".into(),
            "20000".into(),
            "--format".into(),
            "json".into(),
        ];
        args.extend(words.iter().map(|word| (*word).to_owned()));
        cli::parse(&args).unwrap()
    }

    fn run(&self, words: &[&str], harness: &str, session: &str) -> Value {
        self.run_options(self.options(words), harness, session, None)
            .unwrap()
    }

    fn run_options(
        &self,
        options: Arguments,
        harness: &str,
        session: &str,
        body: Option<&str>,
    ) -> Result<Value> {
        let forwarded = board_grammar::normalize_args(&[], &options, body)?;
        let parsed = cli::parse(&forwarded)?;
        let output = self
            .host
            .run(&parsed, &context(harness, session), QueryDeadline::start())?;
        Ok(serde_json::from_str(&output)?)
    }

    fn actor(&self, harness: &str, session: &str) -> BoardActor {
        BoardActor::new(
            "acceptance",
            "edge-host",
            HarnessLabel::parse(harness).unwrap(),
            session,
        )
        .unwrap()
    }

    fn cursor(&self, harness: &str, session: &str) -> u64 {
        let conn = Connection::open(&self.config.db_path).unwrap();
        conn.query_row(
            concat!(
                "SELECT coalesce(s.cursor_seq,0) FROM agent_sessions s JOIN actors a ON a.id=s.actor_id ",
                "WHERE a.user=?1 AND a.host=?2 AND a.harness=?3 AND a.session=?4"
            ),
            rusqlite::params![self.config.user, self.config.host, harness, session],
            |row| row.get::<_, i64>(0),
        ).optional().unwrap().unwrap_or(0).try_into().unwrap()
    }

    fn scalar(&self, sql: &str) -> u64 {
        Connection::open(&self.config.db_path)
            .unwrap()
            .query_row(sql, [], |row| row.get::<_, i64>(0))
            .unwrap()
            .try_into()
            .unwrap()
    }

    fn seed_repo(&self) {
        git(&self.root, &["init", "--initial-branch=main"]);
        std::fs::write(self.root.join("root.txt"), "initial repository\n").unwrap();
        git(&self.root, &["add", "root.txt"]);
        git(&self.root, &["commit", "-m", "Initial root"]);
    }
}

fn context(harness: &str, session: &str) -> RequestContext {
    RequestContext::new(Some(session.into()), Some(harness.into()))
}

fn data(value: &Value) -> &Value {
    value
        .get("result")
        .and_then(|result| result.get("data"))
        .unwrap_or_else(|| panic!("expected typed board result: {value}"))
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "user.name=Board Acceptance",
            "-c",
            "user.email=board@example.invalid",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
