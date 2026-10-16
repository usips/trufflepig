mod checkpoint_progress;
mod checkpoint_scan;
mod checkpoint_storage;
#[cfg(test)]
mod tests;

pub use checkpoint_progress::read as read_index_progress;
pub use checkpoint_progress::{IndexProgress, IndexProgressPhase};

use crate::daemon::deadline::QueryDeadline;
use crate::extract::Extraction;
use crate::store::{Coverage, index_deadline, php_resolver, scan};
use anyhow::{Context, Result};
use checkpoint_progress::{self as progress_io, ProgressRecord};
use checkpoint_storage::CheckpointStorage;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const CHECKPOINT_DIRECTORY: &str = "stage-index-checkpoint";
const CHECKPOINT_DATABASE: &str = "prepared.sqlite3";
const CHECKPOINT_SCHEMA_REVISION: u32 = 1;

pub(super) struct IndexCheckpoint {
    storage: CheckpointStorage,
    cache: PathBuf,
    root: String,
    contract: String,
    base_epoch: String,
    base_generation: i64,
    progress: IndexProgress,
    new_files_this_pass: usize,
    retained_file_count: usize,
    last_progress_write: Instant,
    bounded_slice_started: Option<Instant>,
    bounded_slice_eligible: bool,
}

impl IndexCheckpoint {
    pub(super) fn open(
        conn: &Connection,
        root: &Path,
        cache: &Path,
        deadline: Option<&QueryDeadline>,
    ) -> Result<Self> {
        if let Some(deadline) = deadline {
            index_deadline::check(deadline)?;
        }
        let root = crate::store::encode_path(root);
        let contract = contract_revision();
        let base_epoch: String = conn.query_row(
            "SELECT value FROM meta WHERE key='index_epoch'",
            [],
            |row| row.get(0),
        )?;
        let base_generation: i64 = conn
            .query_row("SELECT value FROM meta WHERE key='generation'", [], |row| {
                row.get::<_, String>(0)
            })?
            .parse()?;
        let cache = cache.canonicalize()?;
        let directory = cache.join(CHECKPOINT_DIRECTORY);
        let database = directory.join(CHECKPOINT_DATABASE);
        let retained = if directory.exists() {
            (|| -> Result<(CheckpointStorage, bool)> {
                let opened = CheckpointStorage::open(&database, deadline)?;
                let valid = opened.metadata("root")?.as_deref() == Some(&root)
                    && opened.metadata("contract")?.as_deref() == Some(&contract)
                    && opened.metadata("base_epoch")?.as_deref() == Some(&base_epoch)
                    && opened.metadata("base_generation")?.as_deref()
                        == Some(base_generation.to_string().as_str());
                if valid {
                    Ok((opened, true))
                } else {
                    drop(opened);
                    Ok((reset_stage(&directory, &database, deadline)?, false))
                }
            })()
        } else {
            std::fs::create_dir(&directory)?;
            CheckpointStorage::open(&database, deadline).map(|storage| (storage, false))
        };
        let (storage, retained_valid) = match retained {
            Ok(retained) => retained,
            Err(error) if checkpoint_storage::is_disposable_stage_error(&error) => {
                (reset_stage(&directory, &database, deadline)?, false)
            }
            Err(error) => return Err(error),
        };

        storage.set_metadata("root", &root)?;
        storage.set_metadata("contract", &contract)?;
        storage.set_metadata("base_epoch", &base_epoch)?;
        storage.set_metadata("base_generation", &base_generation.to_string())?;
        if !retained_valid {
            progress_io::clear(&cache)?;
        }
        let retained_file_count = storage.completed_count()?;
        let previous = retained_valid
            .then(|| progress_io::read_record(&cache))
            .flatten()
            .filter(|record| {
                record.root == root
                    && record.contract == contract
                    && record.base_epoch == base_epoch
                    && record.base_generation == base_generation
            });
        let progress = match previous {
            Some(record) => record.progress,
            None => IndexProgress {
                phase: IndexProgressPhase::Scanning,
                completed_files: retained_file_count,
                reused_files: 0,
                maintenance_required: false,
            },
        };
        Ok(Self {
            storage,
            cache,
            root,
            contract,
            base_epoch,
            base_generation,
            progress,
            new_files_this_pass: 0,
            retained_file_count,
            last_progress_write: Instant::now(),
            bounded_slice_started: None,
            bounded_slice_eligible: false,
        })
    }

    pub(super) fn maintenance_required(&self) -> bool {
        self.progress.maintenance_required
    }

    pub(super) fn has_retained_facts(&self) -> bool {
        self.retained_file_count > 0
    }

    pub(super) fn begin_pass(&mut self) -> Result<()> {
        self.storage.start_scan()?;
        self.new_files_this_pass = 0;
        self.progress.phase = IndexProgressPhase::Scanning;
        self.progress.reused_files = 0;
        self.retained_file_count = self.storage.completed_count()?;
        self.progress.completed_files = self.retained_file_count;
        self.progress.maintenance_required = false;
        self.write_progress()
    }

    pub(super) fn start_bounded_slice(&mut self, deadline: &QueryDeadline) {
        let eligibility = index_deadline::inline_maintenance_eligibility();
        self.bounded_slice_eligible = deadline.remaining() >= eligibility;
        self.bounded_slice_started = Some(Instant::now());
    }

    pub(super) fn extraction(
        &self,
        path: &str,
        revision: Option<&str>,
        source_status: &str,
        bytes: u64,
        extraction_version: i64,
    ) -> Result<Option<Extraction>> {
        self.storage
            .extraction(path, revision, source_status, bytes, extraction_version)
    }

    pub(super) fn reused_file(&mut self, path: &str) -> Result<()> {
        self.storage.mark_seen(path)?;
        self.progress.reused_files = self.progress.reused_files.saturating_add(1);
        self.flush_progress_if_due()
    }

    pub(super) fn completed_file(
        &mut self,
        path: &str,
        revision: Option<&str>,
        source_status: &str,
        bytes: u64,
        extraction_version: i64,
        extraction: &Extraction,
    ) -> Result<()> {
        let new_path = self.storage.save_extraction(
            path,
            revision,
            source_status,
            bytes,
            extraction_version,
            extraction,
        )?;
        self.new_files_this_pass = self.new_files_this_pass.saturating_add(1);
        if new_path {
            self.retained_file_count = self.retained_file_count.saturating_add(1);
            self.progress.completed_files = self.retained_file_count;
        }
        self.storage.mark_seen(path)?;
        self.flush_progress_if_due()
    }

    pub(super) fn pass_completed(&mut self) -> Result<()> {
        self.storage.finish_scan()?;
        self.progress.phase = IndexProgressPhase::Finalizing;
        self.retained_file_count = self.storage.completed_count()?;
        self.progress.completed_files = self.retained_file_count;
        self.write_progress()
    }

    pub(super) fn set_finalizing(&mut self) -> Result<()> {
        self.progress.phase = IndexProgressPhase::Finalizing;
        self.progress.completed_files = self.retained_file_count;
        self.write_progress()
    }

    pub(super) fn note_budget_expired(&mut self, deadline: &QueryDeadline) -> Result<()> {
        self.progress.completed_files = self.retained_file_count;
        let eligibility = index_deadline::inline_maintenance_eligibility();
        let exhausted_eligible_slice = deadline.time_expired()
            && !deadline.cancelled()
            && self.bounded_slice_eligible
            && self
                .bounded_slice_started
                .is_some_and(|started| started.elapsed() >= eligibility);
        if exhausted_eligible_slice && self.new_files_this_pass == 0 {
            self.mark_maintenance_required()
        } else {
            self.write_progress()
        }
    }

    pub(super) fn record_progress(&mut self) -> Result<()> {
        self.progress.completed_files = self.retained_file_count;
        self.write_progress()
    }

    pub(super) fn mark_maintenance_required(&mut self) -> Result<()> {
        self.progress.phase = IndexProgressPhase::MaintenanceRequired;
        self.progress.completed_files = self.retained_file_count;
        self.progress.maintenance_required = true;
        self.write_progress()
    }

    pub(super) fn mark_complete(&mut self) -> Result<()> {
        let directory = self.cache.join(CHECKPOINT_DIRECTORY);
        if directory.exists() {
            std::fs::remove_dir_all(directory).context("remove completed index preparation")?;
        }
        progress_io::clear(&self.cache)
    }

    fn write_progress(&mut self) -> Result<()> {
        progress_io::write(
            &self.cache,
            &ProgressRecord {
                root: self.root.clone(),
                contract: self.contract.clone(),
                base_epoch: self.base_epoch.clone(),
                base_generation: self.base_generation,
                progress: self.progress.clone(),
            },
        )?;
        self.last_progress_write = Instant::now();
        Ok(())
    }

    fn flush_progress_if_due(&mut self) -> Result<()> {
        if self.last_progress_write.elapsed() >= Duration::from_millis(100) {
            self.write_progress()?;
        }
        Ok(())
    }
}

fn reset_stage(
    directory: &Path,
    database: &Path,
    deadline: Option<&QueryDeadline>,
) -> Result<CheckpointStorage> {
    if directory.exists() {
        std::fs::remove_dir_all(directory).context("remove invalid retained index preparation")?;
    }
    std::fs::create_dir(directory).context("recreate retained index preparation")?;
    CheckpointStorage::open(database, deadline)
}

pub(super) fn stage(
    checkpoint: &mut IndexCheckpoint,
    staged: &mut Connection,
    previous: &Connection,
    root: &Path,
    cache: &Path,
    deadline: Option<&QueryDeadline>,
) -> Result<(Coverage, String)> {
    checkpoint_scan::stage(checkpoint, staged, previous, root, cache, deadline)
}

pub(super) fn cleanup_stale_stages(cache: &Path, deadline: Option<&QueryDeadline>) -> Result<()> {
    let retained = cache.join(CHECKPOINT_DIRECTORY);
    for entry in std::fs::read_dir(cache)? {
        if let Some(deadline) = deadline {
            index_deadline::check(deadline)?;
        }
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("stage-")
            || entry.path() == retained
            || !entry.file_type()?.is_dir()
        {
            continue;
        }
        std::fs::remove_dir_all(entry.path())?;
    }
    Ok(())
}

fn contract_revision() -> String {
    let schema = blake3::hash(include_bytes!("schema.rs")).to_hex();
    let references = blake3::hash(include_bytes!("resolve.rs")).to_hex();
    let modules = blake3::hash(include_bytes!("module_resolver.rs")).to_hex();
    let php = blake3::hash(include_bytes!("php_resolver.rs")).to_hex();
    format!(
        "index-checkpoint-{CHECKPOINT_SCHEMA_REVISION};schema-{schema};extractor-{};php-resolver-{}-{php};reference-resolver-{references};module-resolver-{modules}",
        scan::extraction_cache_version(),
        php_resolver::SHARED_RESOLVER_REVISION
    )
}
