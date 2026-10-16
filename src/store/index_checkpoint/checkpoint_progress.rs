use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;

const PROGRESS_FILE: &str = "index-progress.json";
const MAX_PROGRESS_BYTES: u64 = 16 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IndexProgressPhase {
    Scanning,
    Finalizing,
    MaintenanceRequired,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct IndexProgress {
    pub phase: IndexProgressPhase,
    pub completed_files: usize,
    pub reused_files: usize,
    pub maintenance_required: bool,
}

impl IndexProgress {
    pub fn description(&self) -> String {
        match self.phase {
            IndexProgressPhase::Scanning => format!(
                "Checking current source; retained preparation covers {} files and reused {} extracts.",
                self.completed_files, self.reused_files
            ),
            IndexProgressPhase::Finalizing => format!(
                "Prepared {} files and reused {} extracts; finishing index resolution and publication.",
                self.completed_files, self.reused_files
            ),
            IndexProgressPhase::MaintenanceRequired => format!(
                "Prepared {} files and reused {} extracts; run `trufflepig index` to finish maintenance.",
                self.completed_files, self.reused_files
            ),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct ProgressRecord {
    pub root: String,
    pub contract: String,
    pub base_epoch: String,
    pub base_generation: i64,
    pub progress: IndexProgress,
    /// Consecutive expired bounded slices that saved no new file.
    #[serde(default)]
    pub stalled_slices: u32,
}

pub fn read(root: &Path, cache: &Path) -> Option<IndexProgress> {
    let root = root.canonicalize().ok()?;
    let cache = cache.canonicalize().ok()?;
    let record = read_record(&cache)?;
    let stage = cache.join(super::CHECKPOINT_DIRECTORY);
    let stage_metadata = std::fs::symlink_metadata(&stage).ok()?;
    let database_metadata =
        std::fs::symlink_metadata(stage.join(super::CHECKPOINT_DATABASE)).ok()?;
    if record.root != crate::store::encode_path(&root)
        || record.contract != super::contract_revision()
        || !stage_metadata.file_type().is_dir()
        || !database_metadata.file_type().is_file()
    {
        return None;
    }
    let stage_database = rusqlite::Connection::open_with_flags(
        stage.join(super::CHECKPOINT_DATABASE),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    stage_database
        .busy_timeout(std::time::Duration::from_millis(25))
        .ok()?;
    let stage_value = |key: &str| {
        stage_database
            .query_row(
                "SELECT value FROM checkpoint_meta WHERE key=?1",
                [key],
                |row| row.get::<_, String>(0),
            )
            .ok()
    };
    if stage_value("root").as_deref() != Some(record.root.as_str())
        || stage_value("contract").as_deref() != Some(record.contract.as_str())
        || stage_value("base_epoch").as_deref() != Some(record.base_epoch.as_str())
        || stage_value("base_generation").and_then(|value| value.parse::<i64>().ok())
            != Some(record.base_generation)
    {
        return None;
    }
    let index_path = cache.join("index.sqlite3");
    if !std::fs::symlink_metadata(&index_path)
        .ok()?
        .file_type()
        .is_file()
    {
        return None;
    }
    let index = rusqlite::Connection::open_with_flags(
        index_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    index
        .busy_timeout(std::time::Duration::from_millis(25))
        .ok()?;
    let root_record: String = index
        .query_row("SELECT value FROM meta WHERE key='root'", [], |row| {
            row.get(0)
        })
        .ok()?;
    let epoch: String = index
        .query_row(
            "SELECT value FROM meta WHERE key='index_epoch'",
            [],
            |row| row.get(0),
        )
        .ok()?;
    let generation: String = index
        .query_row("SELECT value FROM meta WHERE key='generation'", [], |row| {
            row.get(0)
        })
        .ok()?;
    if root_record != record.root
        || epoch != record.base_epoch
        || generation.parse::<i64>().ok()? != record.base_generation
    {
        return None;
    }
    Some(record.progress)
}

pub(super) fn read_record(cache: &Path) -> Option<ProgressRecord> {
    let cache = cache.canonicalize().ok()?;
    let path = cache.join(PROGRESS_FILE);
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_PROGRESS_BYTES {
        return None;
    }
    let mut file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_PROGRESS_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_PROGRESS_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

pub(super) fn write(cache: &Path, record: &ProgressRecord) -> Result<()> {
    let bytes = serde_json::to_vec(record).context("serialize index progress")?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_PROGRESS_BYTES,
        "index progress too large"
    );
    let mut temporary = tempfile::NamedTempFile::new_in(cache)?;
    std::io::Write::write_all(&mut temporary, &bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(cache.join(PROGRESS_FILE))?;
    std::fs::File::open(cache)?.sync_all()?;
    Ok(())
}

pub(super) fn clear(cache: &Path) -> Result<()> {
    let path = cache.join(PROGRESS_FILE);
    match std::fs::remove_file(path) {
        Ok(()) => std::fs::File::open(cache)?.sync_all()?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
