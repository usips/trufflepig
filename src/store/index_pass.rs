//! One leased indexing pass: observe the tree, return early when it matches
//! the publication, else stage the current files into a disposable database,
//! resolve, and publish the difference into the live index
//! ([coherent publication](../../docs/index-contract.md#coherent-publication)).
use super::index_checkpoint::IndexCheckpoint;
use super::{
    Coverage, Publication, Store, index_checkpoint, index_deadline, live_index, php_resolver,
    publish, regions, scan, schema,
};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use tp_core::path_codec::encode_path;
use tp_core::query_deadline::QueryDeadline;

/// The staged build is discarded on failure, so it keeps no journal and never syncs.
const STAGED_PRAGMAS: &str =
    "PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA cache_size=-4096;";

impl Store {
    /// Stages and publishes while `_writer_lock` holds the writer lease. A
    /// corrupt live index found by an unbounded pass is quarantined and
    /// recreated; the next pass rebuilds it.
    pub(super) fn index_leased(
        &mut self,
        _writer_lock: std::fs::File,
        resolve_staged: impl FnOnce(&mut Connection) -> Result<()>,
        deadline: Option<&QueryDeadline>,
    ) -> Result<Option<Coverage>> {
        let result = self.index_pass(resolve_staged, deadline);
        if let Err(error) = &result
            && deadline.is_none()
            && live_index::is_index_corrupt(error)
            && live_index::fails_quick_check(&self.conn)
        {
            self.replace_corrupt_index()
                .context("quarantine corrupt index after failed pass")?;
        }
        result
    }

    fn replace_corrupt_index(&mut self) -> Result<()> {
        drop(std::mem::replace(
            &mut self.conn,
            Connection::open_in_memory()?,
        ));
        live_index::quarantine(&self.cache)?;
        self.conn = live_index::open_initialized(&self.cache, &self.index_root, None)?;
        Ok(())
    }

    fn index_pass(
        &mut self,
        resolve_staged: impl FnOnce(&mut Connection) -> Result<()>,
        deadline: Option<&QueryDeadline>,
    ) -> Result<Option<Coverage>> {
        let mut checkpoint =
            IndexCheckpoint::open(&self.conn, &self.index_root, &self.cache, deadline)?;
        index_checkpoint::cleanup_stale_stages(&self.cache, deadline)
            .map_err(|error| interrupted(&mut checkpoint, deadline, error))?;
        if deadline.is_some() && checkpoint.maintenance_required() {
            return Ok(None);
        }
        check_slice(&mut checkpoint, deadline)?;
        let capture_started_ms = publish::timestamp_ms()?;
        let observation =
            scan::TreeObservation::observe(&self.conn, &self.index_root, &self.cache, deadline)
                .map_err(|error| interrupted(&mut checkpoint, deadline, error))?;
        if let Some((coverage, scan_fingerprint)) = observation.unchanged_tally() {
            let fingerprint = scan::resolved_fingerprint(
                &scan_fingerprint,
                php_resolver::SHARED_RESOLVER_REVISION,
            );
            let unchanged = self
                .publication_unchanged(&fingerprint, &coverage)
                .and_then(|unchanged| {
                    if unchanged {
                        observation.stat_refresh().apply(&self.conn)?;
                    }
                    Ok(unchanged)
                })
                .map_err(|error| interrupted(&mut checkpoint, deadline, error))?;
            if unchanged {
                checkpoint.mark_complete()?;
                return Ok(Some(coverage));
            }
        }
        let use_checkpoint = deadline.is_some() || checkpoint.has_retained_facts();
        if use_checkpoint {
            checkpoint
                .begin_pass()
                .map_err(|error| interrupted(&mut checkpoint, deadline, error))?;
        }
        check_slice(&mut checkpoint, deadline)?;
        let (directory, mut staged) = open_staged(&self.cache, deadline)
            .map_err(|error| interrupted(&mut checkpoint, deadline, error))?;
        let staged_path = directory.path().join(live_index::INDEX_NAME);
        let regions_current = self
            .regions_current()
            .map_err(|error| interrupted(&mut checkpoint, deadline, error))?;
        let live_index = self.cache.join(live_index::INDEX_NAME);
        let inputs = scan::StageInputs {
            root: &self.index_root,
            live_index: &live_index,
            regions_current,
        };
        let (coverage, scan_fingerprint, stat_refresh) = if use_checkpoint {
            index_checkpoint::stage(
                &mut checkpoint,
                &mut staged,
                &self.conn,
                observation,
                &inputs,
                deadline,
            )?
        } else {
            scan::stage(&mut staged, &self.conn, observation, &inputs, None, None)?
        };
        check_slice(&mut checkpoint, deadline)?;
        let fingerprint =
            scan::resolved_fingerprint(&scan_fingerprint, php_resolver::SHARED_RESOLVER_REVISION);
        let capture_completed_ms = publish::timestamp_ms()?.max(capture_started_ms);
        let unchanged = self
            .publication_unchanged(&fingerprint, &coverage)
            .and_then(|unchanged| {
                if unchanged {
                    stat_refresh.apply(&self.conn)?;
                }
                Ok(unchanged)
            })
            .map_err(|error| interrupted(&mut checkpoint, deadline, error))?;
        if unchanged {
            checkpoint.mark_complete()?;
            return Ok(Some(coverage));
        }
        checkpoint.set_finalizing()?;
        schema::create_staged_indexes(&staged)
            .and_then(|()| resolve_staged(&mut staged))
            .map_err(|error| interrupted(&mut checkpoint, deadline, error))?;
        check_slice(&mut checkpoint, deadline)?;
        drop(staged);
        let (index_epoch, generation) = self
            .next_publication_identity()
            .map_err(|error| interrupted(&mut checkpoint, deadline, error))?;
        let publication = Publication {
            index_epoch,
            generation,
            root: encode_path(&self.index_root),
            capture_started_ms,
            capture_completed_ms,
        };
        if let Some(deadline) = deadline {
            check_slice(&mut checkpoint, Some(deadline))?;
            index_deadline::install(&self.conn, deadline)
                .map_err(|error| interrupted(&mut checkpoint, Some(deadline), error))?;
        }
        publish::publish(
            &mut self.conn,
            &staged_path,
            &coverage,
            &fingerprint,
            &publication,
        )
        .map_err(|error| interrupted(&mut checkpoint, deadline, error))?;
        drop(directory);
        if let Err(error) = live_index::reclaim_after_publish(&self.conn, deadline)
            && !index_deadline::is_deadline_interruption(&error)
        {
            eprintln!("trufflepig: index space reclamation skipped: {error:#}");
        }
        checkpoint
            .mark_complete()
            .context("index published; clearing its preparation failed")?;
        Ok(Some(coverage))
    }

    /// Whether the published index already matches this scan.
    fn publication_unchanged(&self, fingerprint: &str, coverage: &Coverage) -> Result<bool> {
        let previous: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key='fingerprint'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if previous.as_deref() != Some(fingerprint) {
            return Ok(false);
        }
        Ok(self.coverage()?.walk_failures == coverage.walk_failures
            && self.publication()?.is_some())
    }

    /// Whether the live regions were built by this binary's region construction.
    fn regions_current(&self) -> Result<bool> {
        let stored: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key='regions_contract'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(stored.is_some_and(|stored| stored == regions::contract()))
    }

    fn next_publication_identity(&self) -> Result<(String, i64)> {
        let index_epoch = self.conn.query_row(
            "SELECT value FROM meta WHERE key='index_epoch'",
            [],
            |row| row.get(0),
        )?;
        Ok((index_epoch, self.generation()? + 1))
    }
}

/// Creates the disposable staged database inside a fresh `stage-*` directory.
fn open_staged(
    cache: &std::path::Path,
    deadline: Option<&QueryDeadline>,
) -> Result<(tempfile::TempDir, Connection)> {
    let directory = tempfile::Builder::new()
        .prefix("stage-")
        .tempdir_in(cache)?;
    let staged = Connection::open(directory.path().join(live_index::INDEX_NAME))?;
    if let Some(deadline) = deadline {
        index_deadline::install(&staged, deadline)?;
    }
    staged.execute_batch(STAGED_PRAGMAS)?;
    schema::create_staged(&staged)?;
    if let Some(deadline) = deadline {
        index_deadline::check(deadline)?;
    }
    Ok((directory, staged))
}

/// Fails the pass once a bounded slice's deadline has expired.
fn check_slice(checkpoint: &mut IndexCheckpoint, deadline: Option<&QueryDeadline>) -> Result<()> {
    match deadline {
        Some(deadline) => index_deadline::check(deadline)
            .map_err(|error| interrupted(checkpoint, Some(deadline), error)),
        None => Ok(()),
    }
}

/// Records checkpoint progress when a bounded slice ended by expiry, then
/// returns `error`. A failed record is logged; the original error wins.
fn interrupted(
    checkpoint: &mut IndexCheckpoint,
    deadline: Option<&QueryDeadline>,
    error: anyhow::Error,
) -> anyhow::Error {
    if let Some(deadline) = deadline.filter(|deadline| deadline.expired())
        && let Err(record) = checkpoint.note_interruption(deadline, &error)
    {
        eprintln!("trufflepig: index checkpoint progress not recorded: {record:#}");
    }
    error
}
