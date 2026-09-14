//! First-parent history uses captured tips and immutable local Git objects.
mod blame;
#[cfg(test)]
mod command_tests;
mod commands;
mod comparison;
pub mod git;
pub mod source_diff;
mod storage;
mod targets;
#[cfg(test)]
mod tests;
pub mod worker;
mod working_tree;

use crate::{identity::GitOid, output::OutputBudget, store::Store};
use anyhow::Result;
use git::GitRepository;
use rusqlite::Connection;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const MAX_COMMITS: usize = 20_000;
pub const MAX_BLOB_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_DATABASE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_WAL_BYTES: u64 = 256 * 1024 * 1024;

pub struct History {
    pub repository: GitRepository,
    pub(crate) conn: Connection,
    cache: PathBuf,
    pub tip: GitOid,
}

impl History {
    pub fn open(root: &Path, cache: &Path) -> Result<Self> {
        let repository = GitRepository::discover(root)?;
        let tip = repository.resolve("HEAD")?;
        std::fs::create_dir_all(cache)?;
        let conn = Connection::open(cache.join("history.sqlite3"))?;
        let _writer = storage::write_lease(cache)?;
        storage::initialize(&conn, &repository)?;
        Ok(Self {
            repository,
            conn,
            cache: cache.to_owned(),
            tip,
        })
    }

    pub fn index(&mut self) -> Result<Value> {
        storage::index(self)
    }
    pub fn schedule(&self) -> Result<Value> {
        let _writer = storage::write_lease(&self.cache)?;
        storage::schedule(self)?;
        self.status()
    }
    pub fn index_pending(&mut self) -> Result<Value> {
        self.schedule()?;
        use rusqlite::OptionalExtension;
        let pending: Option<String> = self
            .conn
            .query_row(
                "SELECT tip FROM views WHERE complete=0 ORDER BY updated,created,tip LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(tip) = pending {
            self.tip = GitOid::parse(&tip)?;
        }
        self.index()
    }
    pub fn status(&self) -> Result<Value> {
        storage::status(self)
    }
    pub fn hist(&self, store: &mut Store, target: &str, budget: &OutputBudget) -> Result<String> {
        commands::hist(self, store, target, budget)
    }
    pub fn since(
        &self,
        store: &mut Store,
        revision: &str,
        target: Option<&str>,
        uncommitted: bool,
        budget: &OutputBudget,
    ) -> Result<String> {
        commands::since(self, store, revision, target, uncommitted, budget)
    }
    pub fn diff(
        &self,
        store: &mut Store,
        revision: &str,
        target: Option<&str>,
        budget: &OutputBudget,
    ) -> Result<String> {
        commands::diff(self, store, revision, target, budget)
    }
    pub fn blame(
        &self,
        store: &mut Store,
        target: &str,
        raw: bool,
        ignore_revs_file: Option<&Path>,
        budget: &OutputBudget,
    ) -> Result<String> {
        blame::blame(self, store, target, raw, ignore_revs_file, budget)
    }
}
