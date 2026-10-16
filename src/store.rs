//! A per-root WAL index published atomically from an on-disk staging database.

mod cache_access;
pub(crate) mod current_source;
mod index_checkpoint;
mod index_deadline;
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

use crate::daemon::deadline::QueryDeadline;
use crate::{identity::ContentRevision, results::ResultSetStore};
use anyhow::{Context, Result};
pub use cache_access::{
    index_cache_unavailable, is_index_cache_unavailable, preflight_results_cache,
};
pub use index_checkpoint::{IndexProgress, IndexProgressPhase, read_index_progress};
pub use paths::{decode_path, encode_path};
pub use read_access::{is_index_cannot_open, is_index_warming};
use rusqlite::Connection;
pub(crate) use scan::{MAX_SOURCE_BYTES, extraction_cache_version};
pub use seed::{SeedOutcome, ensure_seeded, ensure_seeded_until, read_seed_outcome};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
pub(crate) use worktree_seed_source::linked_worktree_main_checkout;

pub use index_deadline::inline_maintenance_deadline;

const INDEX_LEASE_POLL_INTERVAL: Duration = Duration::from_millis(10);

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
    query_deadline: Option<QueryDeadline>,
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
        Self::open_inner(root, cache, None)
    }

    /// Opens a writer for query-scoped reconciliation with the supplied deadline.
    pub fn open_for_reconcile(root: &Path, cache: &Path, deadline: QueryDeadline) -> Result<Self> {
        Self::open_inner(root, cache, Some(deadline))
    }

    fn open_inner(root: &Path, cache: &Path, deadline: Option<QueryDeadline>) -> Result<Self> {
        if let Some(deadline) = &deadline {
            index_deadline::check(deadline)?;
        }
        let root = root
            .canonicalize()
            .context("canonicalize repository root")?;
        anyhow::ensure!(root.is_dir(), "repository root is not a directory");
        std::fs::create_dir_all(cache)
            .map_err(|error| cache_access::index_cache_unavailable(cache, error.into()))?;
        let cache = cache
            .canonicalize()
            .map_err(|error| cache_access::index_cache_unavailable(cache, error.into()))?;
        anyhow::ensure!(
            !root.starts_with(&cache),
            "cache must not contain the repository root"
        );
        if let Some(deadline) = &deadline {
            index_deadline::check(deadline)?;
        }
        let conn = Connection::open(cache.join("index.sqlite3"))
            .map_err(|error| cache_access::index_cache_unavailable(&cache, error.into()))?;
        let initialized = (|| -> Result<()> {
            if let Some(deadline) = &deadline {
                index_deadline::install(&conn, deadline)?;
            } else {
                conn.busy_timeout(index_deadline::SQLITE_DEFAULT_BUSY_TIMEOUT)?;
            }
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
            if let Some(deadline) = &deadline {
                index_deadline::check(deadline)?;
            }
            Ok(())
        })();
        let initialized = if deadline.is_some() {
            match (initialized, index_deadline::clear(&conn)) {
                (Err(primary), _) => Err(primary),
                (Ok(()), Err(cleanup)) => Err(cleanup.into()),
                (Ok(()), Ok(())) => Ok(()),
            }
        } else {
            initialized
        };
        let initialized = initialized.map_err(|error| {
            let error = cache_access::index_cache_unavailable(&cache, error);
            match &deadline {
                Some(deadline) => index_deadline::classify(deadline, error),
                None => error,
            }
        });
        initialized?;
        Ok(Self {
            conn,
            index_root: root.clone(),
            root,
            results_cache: cache.clone(),
            cache,
            query_deadline: deadline,
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

    /// Makes result-cache reads and writes part of the same bounded query.
    pub fn with_query_deadline(mut self, deadline: QueryDeadline) -> Self {
        self.query_deadline = Some(deadline);
        self.result_sets = std::cell::OnceCell::new();
        self
    }

    /// Returns the caller's unchanged deadline when this store serves a query.
    pub fn query_deadline(&self) -> Option<&QueryDeadline> {
        self.query_deadline.as_ref()
    }

    /// Result sets live in `<results cache>/results.sqlite3`, never in the index,
    /// so saving them never waits behind a publication.
    pub fn result_sets(&self) -> Result<&ResultSetStore> {
        if let Some(opened) = self.result_sets.get() {
            return Ok(opened);
        }
        cache_access::preflight_results_cache(&self.results_cache, self.query_deadline.as_ref())?;
        let opened = match &self.query_deadline {
            Some(deadline) => ResultSetStore::open_with_deadline(&self.results_cache, deadline),
            None => ResultSetStore::open(&self.results_cache),
        }
        .map_err(|error| cache_access::index_cache_unavailable(&self.results_cache, error))?;
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
        self.index_with_resolver(resolve_all)
    }

    /// [`Store::index`], or `None` at once when another writer (a daemon's scan)
    /// holds the writer lease, instead of waiting for it.
    pub fn index_unless_leased(&mut self) -> Result<Option<Coverage>> {
        anyhow::ensure!(
            self.access == StoreAccess::Writer,
            "read_only_store: indexing requires Store::open"
        );
        let writer_lock = self.writer_lease_file()?;
        match fs2::FileExt::try_lock_exclusive(&writer_lock) {
            Ok(()) => self.index_leased(writer_lock, resolve_all, None),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(cache_access::index_cache_unavailable(
                &self.cache,
                error.into(),
            )),
        }
    }

    /// Reconciles within one request deadline, polling briefly for the writer lease.
    pub fn index_until(&mut self, deadline: QueryDeadline) -> Result<Option<Coverage>> {
        self.index_with_deadline(deadline, true)
    }

    /// Makes one bounded indexing attempt without waiting for another writer.
    pub fn index_available_until(&mut self, deadline: QueryDeadline) -> Result<Option<Coverage>> {
        self.index_with_deadline(deadline, false)
    }

    fn index_with_deadline(
        &mut self,
        deadline: QueryDeadline,
        wait_for_writer: bool,
    ) -> Result<Option<Coverage>> {
        anyhow::ensure!(
            self.access == StoreAccess::Writer,
            "read_only_store: indexing requires Store::open_for_reconcile"
        );
        index_deadline::check(&deadline)?;
        let writer_lock = self
            .writer_lease_file()
            .map_err(|error| index_deadline::classify(&deadline, error))?;
        loop {
            index_deadline::check(&deadline)?;
            match fs2::FileExt::try_lock_exclusive(&writer_lock) {
                Ok(()) => {
                    if let Err(error) = index_deadline::install(&self.conn, &deadline) {
                        let _ = index_deadline::clear(&self.conn);
                        return Err(index_deadline::classify(
                            &deadline,
                            cache_access::index_cache_unavailable(&self.cache, error),
                        ));
                    }
                    let result = self
                        .index_leased(writer_lock, resolve_all, Some(&deadline))
                        .map_err(|error| {
                            index_deadline::classify(
                                &deadline,
                                cache_access::index_cache_unavailable(&self.cache, error),
                            )
                        });
                    let clear = index_deadline::clear(&self.conn);
                    return match (result, clear) {
                        (Err(error), _) => Err(error),
                        (Ok(_), Err(error)) => Err(index_deadline::classify(
                            &deadline,
                            cache_access::index_cache_unavailable(&self.cache, error.into()),
                        )),
                        (Ok(coverage), Ok(())) => Ok(coverage),
                    };
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if !wait_for_writer {
                        return Ok(None);
                    }
                    std::thread::sleep(deadline.cap(INDEX_LEASE_POLL_INTERVAL));
                }
                Err(error) => {
                    return Err(index_deadline::classify(
                        &deadline,
                        cache_access::index_cache_unavailable(&self.cache, error.into()),
                    ));
                }
            }
        }
    }

    fn writer_lease_file(&self) -> Result<std::fs::File> {
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.cache.join("index.lock"))
            .map_err(|error| cache_access::index_cache_unavailable(&self.cache, error.into()))
    }

    fn index_with_resolver(
        &mut self,
        resolve_staged: impl FnOnce(&mut Connection) -> Result<()>,
    ) -> Result<Coverage> {
        let writer_lock = self.writer_lease_file()?;
        fs2::FileExt::lock_exclusive(&writer_lock)
            .map_err(|error| cache_access::index_cache_unavailable(&self.cache, error.into()))?;
        self.index_leased(writer_lock, resolve_staged, None)?
            .context("explicit indexing did not produce a publication")
    }

    /// Stages and publishes while `_writer_lock` holds the writer lease.
    fn index_leased(
        &mut self,
        _writer_lock: std::fs::File,
        resolve_staged: impl FnOnce(&mut Connection) -> Result<()>,
        deadline: Option<&QueryDeadline>,
    ) -> Result<Option<Coverage>> {
        let mut checkpoint = index_checkpoint::IndexCheckpoint::open(
            &self.conn,
            &self.index_root,
            &self.cache,
            deadline,
        )?;
        if let Err(error) = index_checkpoint::cleanup_stale_stages(&self.cache, deadline) {
            if deadline.is_some_and(QueryDeadline::expired) {
                record_index_interruption(&mut checkpoint, deadline.unwrap(), false, Some(&error));
            }
            return Err(error);
        }
        if deadline.is_some() && checkpoint.maintenance_required() {
            return Ok(None);
        }
        let use_checkpoint = deadline.is_some() || checkpoint.has_retained_facts();
        if use_checkpoint {
            if let Err(error) = checkpoint.begin_pass() {
                if deadline.is_some_and(QueryDeadline::expired) {
                    record_index_interruption(
                        &mut checkpoint,
                        deadline.unwrap(),
                        false,
                        Some(&error),
                    );
                }
                return Err(error);
            }
        }
        if let Some(deadline) = deadline {
            if let Err(error) = index_deadline::check(deadline) {
                record_index_interruption(&mut checkpoint, deadline, false, None);
                return Err(error);
            }
        }
        let staged_setup = (|| -> Result<_> {
            let directory = tempfile::Builder::new()
                .prefix("stage-")
                .tempdir_in(&self.cache)?;
            let staged_path = directory.path().join("index.sqlite3");
            let staged = Connection::open(&staged_path)?;
            if let Some(deadline) = deadline {
                index_deadline::install(&staged, deadline)?;
                index_deadline::check(deadline)?;
            }
            staged.execute_batch(
                "PRAGMA journal_mode=DELETE; PRAGMA synchronous=OFF; PRAGMA cache_size=-4096;",
            )?;
            schema::create(&staged)?;
            if let Some(deadline) = deadline {
                index_deadline::check(deadline)?;
            }
            let capture_started_ms = publish::timestamp_ms()?;
            Ok((directory, staged, capture_started_ms))
        })();
        let (directory, mut staged, capture_started_ms) = match staged_setup {
            Ok(setup) => setup,
            Err(error) => {
                if deadline.is_some_and(QueryDeadline::expired) {
                    record_index_interruption(
                        &mut checkpoint,
                        deadline.unwrap(),
                        false,
                        Some(&error),
                    );
                }
                return Err(error);
            }
        };
        if let Some(deadline) = deadline {
            checkpoint.start_bounded_slice(deadline);
        }
        let staged_path = directory.path().join("index.sqlite3");
        let (coverage, scan_fingerprint) = if use_checkpoint {
            index_checkpoint::stage(
                &mut checkpoint,
                &mut staged,
                &self.conn,
                &self.index_root,
                &self.cache,
                deadline,
            )?
        } else {
            scan::stage(&mut staged, &self.conn, &self.index_root, &self.cache)?
        };
        if let Some(deadline) = deadline {
            if let Err(error) = index_deadline::check(deadline) {
                record_index_interruption(&mut checkpoint, deadline, true, None);
                return Err(error);
            }
        }
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
            .optional()
            .map_err(|error| {
                let error = anyhow::Error::from(error);
                if deadline.is_some_and(QueryDeadline::expired) {
                    record_index_interruption(
                        &mut checkpoint,
                        deadline.unwrap(),
                        true,
                        Some(&error),
                    );
                }
                error
            })?;
        let unchanged = if previous.as_deref() == Some(&fingerprint) {
            (|| -> Result<bool> {
                Ok(self.coverage()?.walk_failures == coverage.walk_failures
                    && self.publication()?.is_some())
            })()
        } else {
            Ok(false)
        };
        let unchanged = match unchanged {
            Ok(unchanged) => unchanged,
            Err(error) => {
                if deadline.is_some_and(QueryDeadline::expired) {
                    record_index_interruption(
                        &mut checkpoint,
                        deadline.unwrap(),
                        true,
                        Some(&error),
                    );
                }
                return Err(error);
            }
        };
        if unchanged {
            let _ = checkpoint.mark_complete();
            return Ok(Some(coverage));
        }
        checkpoint.set_finalizing()?;
        if let Err(error) = resolve_staged(&mut staged) {
            if deadline.is_some_and(QueryDeadline::expired) {
                record_index_interruption(&mut checkpoint, deadline.unwrap(), true, Some(&error));
            }
            return Err(error);
        }
        if let Some(deadline) = deadline {
            if let Err(error) = index_deadline::check(deadline) {
                record_index_interruption(&mut checkpoint, deadline, true, None);
                return Err(error);
            }
        }
        drop(staged);
        let publication_identity = (|| -> Result<(String, i64)> {
            let index_epoch = self.conn.query_row(
                "SELECT value FROM meta WHERE key='index_epoch'",
                [],
                |row| row.get(0),
            )?;
            Ok((index_epoch, self.generation()? + 1))
        })();
        let (index_epoch, generation) = match publication_identity {
            Ok(identity) => identity,
            Err(error) => {
                if deadline.is_some_and(QueryDeadline::expired) {
                    record_index_interruption(
                        &mut checkpoint,
                        deadline.unwrap(),
                        true,
                        Some(&error),
                    );
                }
                return Err(error);
            }
        };
        let publication = Publication {
            index_epoch,
            generation,
            root: encode_path(&self.index_root),
            capture_started_ms,
            capture_completed_ms,
        };
        if let Some(deadline) = deadline {
            if let Err(error) = index_deadline::check(deadline) {
                record_index_interruption(&mut checkpoint, deadline, true, None);
                return Err(error);
            }
            if let Err(error) = index_deadline::install(&self.conn, deadline) {
                if deadline.expired() {
                    record_index_interruption(&mut checkpoint, deadline, true, Some(&error));
                }
                return Err(error);
            }
        }
        if let Err(error) = publish::publish(
            &mut self.conn,
            &staged_path,
            &coverage,
            &fingerprint,
            &publication,
        ) {
            if deadline.is_some_and(QueryDeadline::expired) {
                record_index_interruption(&mut checkpoint, deadline.unwrap(), true, Some(&error));
            }
            return Err(error);
        }
        let _ = checkpoint.mark_complete();
        Ok(Some(coverage))
    }
}

fn record_index_interruption(
    checkpoint: &mut index_checkpoint::IndexCheckpoint,
    deadline: &QueryDeadline,
    _finalization: bool,
    cause: Option<&anyhow::Error>,
) {
    let exhausted = deadline.time_expired()
        && !deadline.cancelled()
        && cause.is_none_or(index_deadline::is_deadline_interruption);
    let update = if exhausted {
        checkpoint.note_budget_expired(deadline)
    } else {
        checkpoint.record_progress()
    };
    let _ = update;
}

/// The full resolver pass over a staged index.
fn resolve_all(staged: &mut Connection) -> Result<()> {
    resolve::references(staged)?;
    php_resolver::resolve(staged)?;
    module_resolver::resolve(staged)
}

use rusqlite::OptionalExtension;
