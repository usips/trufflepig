//! A per-root WAL index published atomically from an on-disk staging database.

mod inheritance;
#[cfg(test)]
mod inheritance_cache_tests;
mod module_config;
mod module_resolver;
mod paths;
mod php_resolver;
#[cfg(test)]
mod publication_tests;
mod publish;
mod read_access;
mod regions;
mod resolve;
mod scan;
mod schema;
pub mod seed;
#[cfg(test)]
mod seed_tests;
#[cfg(test)]
mod tests;
mod worktree_seed_source;

use crate::{identity::ContentRevision, results::ResultSetStore};
use anyhow::{Context, Result};
pub use paths::{decode_path, encode_path};
pub use read_access::is_index_warming;
use rusqlite::Connection;
pub(crate) use scan::{MAX_SOURCE_BYTES, extraction_cache_version};
pub use seed::{SeedOutcome, ensure_seeded, read_seed_outcome};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
pub(crate) use worktree_seed_source::linked_worktree_main_checkout;

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

/// One published scan of the configured root; files were observed over an interval.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Publication {
    pub index_epoch: String,
    pub generation: i64,
    pub root: String,
    pub capture_started_ms: u64,
    pub capture_completed_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PublishedFile {
    pub path: String,
    pub revision: Option<ContentRevision>,
    pub status: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PublishedFiles {
    pub publication: Publication,
    pub files: Vec<PublishedFile>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PublicationChange {
    pub generation: i64,
    pub path: String,
    pub before_revision: Option<ContentRevision>,
    pub after_revision: Option<ContentRevision>,
    pub before_status: Option<String>,
    pub after_status: Option<String>,
    pub kind: String,
}

/// Completeness describes published observations; edits between scans remain unseen.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PublicationObservations {
    pub from_generation: i64,
    pub to_generation: i64,
    pub retained_generation_floor: i64,
    pub complete: bool,
    pub changes: Vec<PublicationChange>,
}

/// A per-root index connection. [`Store::open`] is the writer (index, init,
/// sessions, reconciler); [`Store::open_read`] is the query-only reader, which
/// may read bytes from another checkout via [`Store::reading_from`].
pub struct Store {
    pub conn: Connection,
    /// Root whose current bytes reads verify and render.
    pub root: PathBuf,
    /// Root the published index describes; equals `root` unless reading from a worktree.
    index_root: PathBuf,
    cache: PathBuf,
    /// Cache holding this store's `results.sqlite3`.
    results_cache: PathBuf,
    result_sets: std::cell::OnceCell<ResultSetStore>,
    access: StoreAccess,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StoreAccess {
    Writer,
    Reader,
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
        publish::create_journal(&conn)?;
        conn.execute(
            "INSERT OR IGNORE INTO meta(key,value) VALUES('index_epoch',?1)",
            [uuid::Uuid::new_v4().to_string()],
        )?;
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
        Ok(Self {
            conn,
            index_root: root.clone(),
            root,
            results_cache: cache.clone(),
            cache,
            result_sets: std::cell::OnceCell::new(),
            access: StoreAccess::Writer,
        })
    }

    /// The root this index describes; differs from `root` for a worktree view.
    pub fn index_root(&self) -> &Path {
        &self.index_root
    }

    /// The cache holding `index.sqlite3`.
    pub fn index_cache(&self) -> &Path {
        &self.cache
    }

    /// The cache holding `results.sqlite3`; the read root's own cache in a
    /// worktree view, otherwise [`Store::index_cache`].
    pub fn results_cache(&self) -> &Path {
        &self.results_cache
    }

    /// Result sets live in `<results cache>/results.sqlite3`, never in the index,
    /// so saving them never waits behind a publication.
    pub fn result_sets(&self) -> Result<&ResultSetStore> {
        if let Some(opened) = self.result_sets.get() {
            return Ok(opened);
        }
        let opened = ResultSetStore::open(&self.results_cache)?;
        Ok(self.result_sets.get_or_init(|| opened))
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

    /// Returns no interval for an index that has never published a captured scan.
    pub fn publication(&self) -> Result<Option<Publication>> {
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key='publication'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        value
            .map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose()
    }

    /// Reads metadata and file revisions/content from the same SQLite snapshot.
    /// Publication is local to this database, not atomic with any history database.
    pub fn with_publication<T>(
        &self,
        read: impl FnOnce(&Connection, &Publication) -> Result<T>,
    ) -> Result<T> {
        let transaction = self.conn.unchecked_transaction()?;
        let publication = self
            .publication()?
            .context("working_tree_unavailable: no published capture")?;
        let result = read(&transaction, &publication)?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn published_files(&self) -> Result<PublishedFiles> {
        self.with_publication(|conn, publication| {
            let count: i64 = conn.query_row("SELECT count(*) FROM files", [], |row| row.get(0))?;
            let mut files = Vec::with_capacity(count.try_into()?);
            let mut statement =
                conn.prepare("SELECT path,revision,status,bytes FROM files ORDER BY path")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let revision: Option<String> = row.get(1)?;
                files.push(PublishedFile {
                    path: row.get(0)?,
                    revision: revision
                        .as_deref()
                        .map(ContentRevision::parse)
                        .transpose()?,
                    status: row.get(2)?,
                    bytes: row.get::<_, i64>(3)?.try_into()?,
                });
            }
            Ok(PublishedFiles {
                publication: publication.clone(),
                files,
            })
        })
    }

    pub fn observations_since(&self, generation: i64) -> Result<PublicationObservations> {
        self.with_publication(|conn, publication| {
            publish::observations_since(conn, generation, publication.generation)
        })
    }

    pub fn index(&mut self) -> Result<Coverage> {
        anyhow::ensure!(
            self.access == StoreAccess::Writer,
            "read_only_store: indexing requires Store::open"
        );
        self.index_with_resolver(|staged| {
            resolve::references(staged)?;
            php_resolver::resolve(staged)?;
            module_resolver::resolve(staged)
        })
    }

    fn index_with_resolver(
        &mut self,
        resolve_staged: impl FnOnce(&mut Connection) -> Result<()>,
    ) -> Result<Coverage> {
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
        let capture_started_ms = publish::timestamp_ms()?;
        let (coverage, scan_fingerprint) =
            scan::stage(&mut staged, &self.conn, &self.index_root, &self.cache)?;
        let fingerprint =
            scan::resolved_fingerprint(&scan_fingerprint, php_resolver::SHARED_RESOLVER_REVISION);
        let capture_completed_ms = publish::timestamp_ms()?.max(capture_started_ms);
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
            && self.publication()?.is_some()
        {
            return Ok(coverage);
        }
        resolve_staged(&mut staged)?;
        drop(staged);
        let publication = Publication {
            index_epoch: self.conn.query_row(
                "SELECT value FROM meta WHERE key='index_epoch'",
                [],
                |row| row.get(0),
            )?,
            generation: self.generation()? + 1,
            root: encode_path(&self.index_root),
            capture_started_ms,
            capture_completed_ms,
        };
        publish::publish(
            &mut self.conn,
            &staged_path,
            &coverage,
            &fingerprint,
            &publication,
        )?;
        Ok(coverage)
    }
}

use rusqlite::OptionalExtension;
