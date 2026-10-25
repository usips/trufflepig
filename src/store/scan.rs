//! Stages an observed tree into a disposable database: every file's facts,
//! plus bytes, extraction cache rows, and lexical regions for each file whose
//! published revision does not stand
//! ([staged build](../../docs/index-storage-contract.md#staged-build)).
mod extraction_reuse;
mod fact_rows;
mod file_stat;
mod scan_tally;
mod tree_observation;

pub(crate) use extraction_reuse::extraction_cache_version;
pub(super) use extraction_reuse::grammar;
pub(super) use fact_rows::{insert_facts, insert_module_definition};
#[cfg(test)]
pub(super) use file_stat::TEST_PASS_START_SKEW_NS;
pub(super) use tree_observation::{StatRefresh, TreeObservation};

use super::{Coverage, index_checkpoint::IndexCheckpoint, index_deadline, regions};
use crate::extract::{self, Extraction};
use crate::query_deadline::QueryDeadline;
use anyhow::{Context, Result};
use extraction_reuse::{cached_extraction, insert_extraction_cache, published_extraction};
use file_stat::FileStat;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use scan_tally::ScanTally;
use std::path::Path;
use tree_observation::{FileSource, ObservedFile};

pub(crate) const MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024;

#[cfg(test)]
thread_local! {
    /// Test counter: source files read from disk by this thread.
    pub(super) static TEST_SOURCE_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Where a pass stages: the live index it reuses from, and whether its regions are current.
pub(super) struct StageInputs<'a> {
    pub(super) root: &'a Path,
    pub(super) live_index: &'a Path,
    pub(super) regions_current: bool,
}

/// Stages `observation` into `conn`; `previous` is the live index. Returns the
/// coverage, scan fingerprint, and stats to record if the pass publishes nothing.
pub(super) fn stage(
    conn: &mut Connection,
    previous: &Connection,
    observation: TreeObservation,
    inputs: &StageInputs<'_>,
    deadline: Option<&QueryDeadline>,
    mut checkpoint: Option<&mut IndexCheckpoint>,
) -> Result<(Coverage, String, StatRefresh)> {
    if let Some(deadline) = deadline {
        index_deadline::check(deadline)?;
    }
    let mut refresh = StatRefresh::default();
    let cache_version = extraction_cache_version();
    let mut tally = ScanTally::new();
    let transaction = conn.transaction()?;
    for mut file in observation.files {
        if let Some(deadline) = deadline {
            index_deadline::check(deadline)?;
        }
        if matches!(file.source, FileSource::Pending) {
            file.read_pending(inputs.root);
        }
        if let Some(deadline) = deadline {
            index_deadline::check(deadline)?;
        }
        let extraction =
            file_extraction(&file, previous, cache_version, checkpoint.as_deref_mut())?;
        if let Some(deadline) = deadline {
            index_deadline::check(deadline)?;
        }
        let stable = inputs.regions_current
            && extraction.served
            && file.published.as_ref().is_some_and(|published| {
                published.revision == file.revision && published.status != "cancelled"
            });
        let extraction = extraction.facts;
        refresh.note(&file);
        tally.file(
            &file.path,
            file.read_status,
            file.length,
            file.revision.as_deref(),
            &extraction.status,
            file.revision.is_some(),
        );
        let file_id = insert_file(&transaction, &file, &extraction)?;
        if !stable {
            transaction
                .prepare_cached("INSERT INTO restaged(file_id) VALUES(?1)")?
                .execute([file_id])?;
        }
        let Some(revision) = file.revision.as_deref() else {
            continue;
        };
        let loaded;
        let source = match &file.source {
            FileSource::Read(source) => Some(source.as_slice()),
            _ if stable => None,
            _ => {
                loaded = published_source(previous, revision)?;
                Some(loaded.as_slice())
            }
        };
        if let (false, Some(source)) = (stable, source) {
            if extraction.status != "cancelled" {
                insert_extraction_cache(
                    &transaction,
                    revision,
                    &file.path,
                    cache_version,
                    &extraction,
                )?;
            }
            transaction
                .prepare_cached("INSERT OR IGNORE INTO contents(revision,bytes) VALUES(?1,?2)")?
                .execute(params![revision, source])?;
        }
        insert_facts(&transaction, file_id, &extraction)?;
        if extraction.language != "text" {
            insert_module_definition(&transaction, file_id, &file.path, file.length as usize)?;
        }
        if let (false, Some(source)) = (stable, source) {
            regions::insert(
                &transaction,
                file_id,
                &file.path,
                source,
                &extraction.definitions,
            )?;
        }
    }
    if let Some(deadline) = deadline {
        index_deadline::check(deadline)?;
    }
    transaction.commit()?;
    copy_published_contents(conn, inputs.live_index)?;
    if let Some(checkpoint) = checkpoint
        && observation.walk_failures == 0
    {
        checkpoint.pass_completed()?;
    }
    let (coverage, fingerprint) = tally.finish(observation.walk_failures);
    Ok((coverage, fingerprint, refresh))
}

/// A file's facts and whether durable facts served them unchanged.
struct FileExtraction {
    facts: Extraction,
    served: bool,
}

fn file_extraction(
    file: &ObservedFile,
    previous: &Connection,
    cache_version: i64,
    checkpoint: Option<&mut IndexCheckpoint>,
) -> Result<FileExtraction> {
    let mut checkpoint = checkpoint;
    let revision = file.revision.as_deref();
    if let Some(checkpoint) = checkpoint.as_deref_mut()
        && let Some(facts) = checkpoint.extraction(
            &file.path,
            revision,
            file.read_status,
            file.length,
            cache_version,
        )?
    {
        checkpoint.reused_file(&file.path)?;
        return Ok(FileExtraction {
            facts,
            served: true,
        });
    }
    let Some(revision) = revision else {
        let facts = Extraction {
            language: extract::language(&file.path).to_owned(),
            status: file.read_status.to_owned(),
            ..Extraction::default()
        };
        return Ok(FileExtraction {
            facts,
            served: true,
        });
    };
    let (facts, served) = match &file.source {
        FileSource::Read(source) => {
            cached_extraction(previous, &file.path, revision, source, cache_version)?
        }
        _ => match published_extraction(previous, &file.path, revision, cache_version)? {
            Some(facts) => (facts, true),
            None => {
                let source = published_source(previous, revision)?;
                cached_extraction(previous, &file.path, revision, &source, cache_version)?
            }
        },
    };
    if let Some(checkpoint) = checkpoint {
        if served {
            checkpoint.publication_reused_file()?;
        } else if facts.status != "cancelled" && checkpoint.retains_new_extractions() {
            checkpoint.completed_file(
                &file.path,
                Some(revision),
                file.read_status,
                file.length,
                cache_version,
                &facts,
            )?;
        }
    }
    Ok(FileExtraction { facts, served })
}

fn insert_file(tx: &Transaction<'_>, file: &ObservedFile, extraction: &Extraction) -> Result<i64> {
    let [size, mtime, ctime, inode, device] = FileStat::columns(file.stat);
    tx.prepare_cached(
        "INSERT INTO files(path,revision,language,status,bytes,stat_size,stat_mtime_ns,stat_ctime_ns,stat_inode,stat_device)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
    )?
    .execute(params![
        file.path,
        file.revision,
        extraction.language,
        extraction.status,
        file.length as i64,
        size,
        mtime,
        ctime,
        inode,
        device
    ])?;
    Ok(tx.last_insert_rowid())
}

/// The published bytes of `revision`; observation reuses a revision only while they exist.
fn published_source(previous: &Connection, revision: &str) -> Result<Vec<u8>> {
    previous
        .prepare_cached("SELECT bytes FROM contents WHERE revision=?1")?
        .query_row([revision], |row| row.get(0))
        .optional()?
        .with_context(|| format!("published contents missing for revision {revision}"))
}

/// Copies the published bytes of every staged revision not read this pass, so
/// resolvers that read configuration and PHP sources see the complete tree.
fn copy_published_contents(conn: &Connection, live_index: &Path) -> Result<()> {
    let live_index = live_index.to_str().context("non-UTF8 cache directory")?;
    conn.execute("ATTACH DATABASE ?1 AS live_index", [live_index])?;
    let copied = conn.execute_batch(
        "INSERT OR IGNORE INTO main.contents(revision,bytes)
         SELECT revision,bytes FROM live_index.contents
         WHERE revision IN (SELECT revision FROM main.files WHERE revision IS NOT NULL)
           AND revision NOT IN (SELECT revision FROM main.contents);",
    );
    let detached = conn.execute_batch("DETACH DATABASE live_index");
    copied?;
    Ok(detached?)
}

pub(crate) fn resolved_fingerprint(scan_fingerprint: &str, resolver_revision: &str) -> String {
    let mut fingerprint = blake3::Hasher::new();
    fingerprint.update(b"trufflepig-resolved-index-v1");
    fingerprint.update(scan_fingerprint.as_bytes());
    fingerprint.update(resolver_revision.as_bytes());
    fingerprint.finalize().to_hex().to_string()
}

fn read_source(
    root: &Path,
    relative: &Path,
    metadata: &std::fs::Metadata,
) -> (Option<Vec<u8>>, &'static str, u64) {
    let length = metadata.len();
    if length > MAX_SOURCE_BYTES {
        return (None, "resource_excluded", length);
    }
    #[cfg(test)]
    TEST_SOURCE_READS.set(TEST_SOURCE_READS.get() + 1);
    let source = match crate::source::read_contained(root, relative, MAX_SOURCE_BYTES as usize) {
        Ok(source) => source,
        Err(error) => {
            return (
                None,
                if crate::coded_error::ErrorCode::SourceExcluded.is_code_of(&error) {
                    "resource_excluded"
                } else {
                    "read_error"
                },
                length,
            );
        }
    };
    if source.contains(&0) {
        return (None, "binary_excluded", source.len() as u64);
    }
    let length = source.len() as u64;
    (Some(source), "read", length)
}
