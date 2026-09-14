//! A per-root WAL index published atomically from an on-disk staging database.

mod module_config;
mod module_resolver;
mod paths;
mod publish;
mod regions;
mod resolve;
mod scan;
mod schema;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result};
pub use paths::{decode_path, encode_path};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Coverage {
    pub total_files: usize,
    pub indexed_files: usize,
    pub excluded_files: usize,
    pub parse_failures: usize,
    pub semantic_files: usize,
    pub walk_failures: usize,
    pub truncated_files: usize,
    pub indexed_bytes: u64,
}

pub struct Store {
    pub conn: Connection,
    pub root: PathBuf,
    cache: PathBuf,
}

impl Store {
    pub fn open(root: &Path, cache: &Path) -> Result<Self> {
        let root = root
            .canonicalize()
            .context("canonicalize repository root")?;
        anyhow::ensure!(root.is_dir(), "repository root is not a directory");
        std::fs::create_dir_all(cache)?;
        let cache = cache.canonicalize()?;
        anyhow::ensure!(
            !root.starts_with(&cache),
            "cache must not contain the repository root"
        );
        let conn = Connection::open(cache.join("index.sqlite3"))?;
        conn.busy_timeout(std::time::Duration::from_secs(30))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA cache_size=-8192; PRAGMA foreign_keys=ON;")?;
        schema::create(&conn)?;
        let stored_root: Option<String> = conn
            .query_row("SELECT value FROM meta WHERE key='root'", [], |row| {
                row.get(0)
            })
            .optional()?;
        let encoded_root = encode_path(&root);
        anyhow::ensure!(
            stored_root
                .as_ref()
                .is_none_or(|stored| stored == &encoded_root),
            "cache belongs to another repository root"
        );
        conn.execute(
            "INSERT OR IGNORE INTO meta(key,value) VALUES('root',?1)",
            [&encoded_root],
        )?;
        Ok(Self { conn, root, cache })
    }

    pub fn generation(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key='generation'", [], |row| {
                row.get::<_, String>(0)
            })?
            .parse()?)
    }

    pub fn coverage(&self) -> Result<Coverage> {
        let value: String =
            self.conn
                .query_row("SELECT value FROM meta WHERE key='coverage'", [], |row| {
                    row.get(0)
                })?;
        Ok(serde_json::from_str(&value)?)
    }

    pub fn index(&mut self) -> Result<Coverage> {
        let writer_lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.cache.join("index.lock"))?;
        fs2::FileExt::lock_exclusive(&writer_lock)?;
        // The writer lease makes any previous staging directory an interrupted scan.
        for entry in std::fs::read_dir(&self.cache)? {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("stage-"))
            {
                std::fs::remove_dir_all(entry.path())?;
            }
        }
        let directory = tempfile::Builder::new()
            .prefix("stage-")
            .tempdir_in(&self.cache)?;
        let staged_path = directory.path().join("index.sqlite3");
        let mut staged = Connection::open(&staged_path)?;
        staged.execute_batch(
            "PRAGMA journal_mode=DELETE; PRAGMA synchronous=OFF; PRAGMA cache_size=-4096;",
        )?;
        schema::create(&staged)?;
        let (coverage, fingerprint) =
            scan::stage(&mut staged, &self.conn, &self.root, &self.cache)?;
        let previous: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key='fingerprint'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if previous.as_deref() == Some(&fingerprint)
            && self.coverage()?.walk_failures == coverage.walk_failures
        {
            return Ok(coverage);
        }
        resolve::references(&mut staged)?;
        module_resolver::resolve(&mut staged)?;
        drop(staged);
        publish::publish(&mut self.conn, &staged_path, &coverage, &fingerprint)?;
        Ok(coverage)
    }
}

use rusqlite::OptionalExtension;
