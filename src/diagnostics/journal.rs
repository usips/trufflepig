use super::{DiagnosticsMode, RequestEvent, records, sessions};
use anyhow::{Result, ensure};
use rusqlite::Connection;
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub(super) const RETENTION_SECONDS: u64 = 30 * 24 * 60 * 60;
const SEGMENT_BYTES: u64 = 50 * 1024 * 1024;
const TOTAL_BYTES: u64 = 200 * 1024 * 1024;
const DATABASE_BYTES: u64 = 72 * 1024 * 1024;
const RECORD_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecordStatus {
    Recorded,
    Disabled,
    Dropped,
    Invalidated,
}

pub struct DiagnosticStore {
    pub(super) conn: Connection,
    pub(super) directory: PathBuf,
    mode: DiagnosticsMode,
    epoch: i64,
    pub(super) local_drops: AtomicU64,
}

impl DiagnosticStore {
    pub fn open(cache: &Path, mode: DiagnosticsMode) -> Result<Self> {
        let directory = cache.join("diagnostics");
        std::fs::create_dir_all(&directory)?;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        let database = directory.join("sessions.sqlite3");
        private_open(&database, false)?;
        let conn = Connection::open(database)?;
        conn.busy_timeout(Duration::from_millis(10))?;
        let page_size: i64 = conn.query_row("PRAGMA page_size", [], |row| row.get(0))?;
        conn.pragma_update(None, "max_page_count", DATABASE_BYTES as i64 / page_size)?;
        conn.execute_batch(
            "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS diagnostic_meta(key TEXT PRIMARY KEY,value INTEGER NOT NULL);
            INSERT OR IGNORE INTO diagnostic_meta VALUES('epoch',0);
            INSERT OR IGNORE INTO diagnostic_meta VALUES('drops',0);
            INSERT OR IGNORE INTO diagnostic_meta VALUES('forgotten_micros',0);
            INSERT OR IGNORE INTO diagnostic_meta VALUES('evicted',0);",
        )?;
        sessions::create_schema(&conn)?;
        let epoch = conn.query_row(
            "SELECT value FROM diagnostic_meta WHERE key='epoch'",
            [],
            |r| r.get(0),
        )?;
        Ok(Self {
            conn,
            directory,
            mode,
            epoch,
            local_drops: AtomicU64::new(0),
        })
    }

    pub fn record(&self, event: &RequestEvent) -> Result<RecordStatus> {
        if self.mode == DiagnosticsMode::Off {
            return Ok(RecordStatus::Disabled);
        }
        let Some(_guard) = self.lock()? else {
            self.local_drops.fetch_add(1, Ordering::Relaxed);
            return Ok(RecordStatus::Dropped);
        };
        let epoch: i64 = self.conn.query_row(
            "SELECT value FROM diagnostic_meta WHERE key='epoch'",
            [],
            |r| r.get(0),
        )?;
        if epoch != self.epoch {
            return Ok(RecordStatus::Invalidated);
        }
        let forgotten: i64 = self.conn.query_row(
            "SELECT value FROM diagnostic_meta WHERE key='forgotten_micros'",
            [],
            |row| row.get(0),
        )?;
        if event.context.created_unix_micros <= forgotten as u64 {
            return Ok(RecordStatus::Invalidated);
        }
        if let Some(session) = &event.context.session {
            let exists: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM diagnostic_sessions WHERE id=?1)",
                [session],
                |row| row.get(0),
            )?;
            if !exists {
                return Ok(RecordStatus::Invalidated);
            }
        }
        let mut event = event.clone();
        records::sanitize(&mut event, self.mode);
        let mut bytes = serde_json::to_vec(&event)?;
        while bytes.len() + 1 > RECORD_BYTES && !event.emitted.is_empty() {
            event.emitted.pop();
            event.truncated = true;
            bytes = serde_json::to_vec(&event)?;
        }
        ensure!(
            bytes.len() < RECORD_BYTES,
            "diagnostic record exceeds bound"
        );
        bytes.push(b'\n');
        let mut segments = self.segments()?;
        self.prune(&mut segments, bytes.len() as u64)?;
        let active = segments
            .last()
            .filter(|path| {
                path.metadata()
                    .is_ok_and(|m| m.len() + bytes.len() as u64 <= SEGMENT_BYTES)
            })
            .cloned()
            .unwrap_or_else(|| {
                self.directory.join(format!(
                    "events-{:020}-{}.jsonl",
                    records::now(),
                    uuid::Uuid::new_v4()
                ))
            });
        private_open(&active, true)?.write_all(&bytes)?;
        self.persist_drops()?;
        Ok(RecordStatus::Recorded)
    }

    pub(super) fn persist_drops(&self) -> Result<()> {
        let drops = self.local_drops.swap(0, Ordering::Relaxed);
        let epoch: i64 = self.conn.query_row(
            "SELECT value FROM diagnostic_meta WHERE key='epoch'",
            [],
            |row| row.get(0),
        )?;
        if epoch != self.epoch {
            return Ok(());
        }
        if let Err(error) = self.conn.execute(
            "UPDATE diagnostic_meta SET value=value+?1 WHERE key='drops'",
            [drops.min(i64::MAX as u64) as i64],
        ) {
            self.local_drops.fetch_add(drops, Ordering::Relaxed);
            return Err(error.into());
        }
        Ok(())
    }

    pub fn start_session(&self, store: &mut crate::store::Store) -> Result<serde_json::Value> {
        ensure!(self.mode != DiagnosticsMode::Off, "diagnostics_disabled");
        let _guard = self
            .lock()?
            .ok_or_else(|| anyhow::anyhow!("diagnostics_busy"))?;
        let growth = DATABASE_BYTES
            .saturating_sub(self.directory.join("sessions.sqlite3").metadata()?.len());
        self.prune(&mut self.segments()?, growth)?;
        sessions::start(&self.conn, store)
    }

    pub fn end_session(
        &self,
        store: &mut crate::store::Store,
        id: &str,
    ) -> Result<serde_json::Value> {
        let _guard = self
            .lock()?
            .ok_or_else(|| anyhow::anyhow!("diagnostics_busy"))?;
        let growth = DATABASE_BYTES
            .saturating_sub(self.directory.join("sessions.sqlite3").metadata()?.len());
        self.prune(&mut self.segments()?, growth)?;
        sessions::end(&self.conn, store, id)
    }

    pub fn forget(&self) -> Result<()> {
        let _guard = self
            .lock()?
            .ok_or_else(|| anyhow::anyhow!("diagnostics_busy"))?;
        let transaction = self.conn.unchecked_transaction()?;
        transaction.execute(
            "UPDATE diagnostic_meta SET value=value+1 WHERE key='epoch'",
            [],
        )?;
        let forgotten = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros()
            .min(i64::MAX as u128) as i64;
        transaction.execute(
            "UPDATE diagnostic_meta SET value=?1 WHERE key='forgotten_micros'",
            [forgotten],
        )?;
        sessions::forget(&transaction)?;
        transaction.execute(
            "UPDATE diagnostic_meta SET value=0 WHERE key IN ('drops','evicted')",
            [],
        )?;
        transaction.commit()?;
        for segment in self.segments()? {
            std::fs::remove_file(segment)?;
        }
        self.conn.execute_batch("VACUUM")?;
        Ok(())
    }

    pub(super) fn segments(&self) -> Result<Vec<PathBuf>> {
        let mut paths = Vec::with_capacity(5);
        for entry in std::fs::read_dir(&self.directory)? {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|s| s.starts_with("events-") && s.ends_with(".jsonl"))
            {
                paths.push(entry.path());
            }
        }
        paths.sort();
        Ok(paths)
    }

    fn prune(&self, segments: &mut Vec<PathBuf>, incoming: u64) -> Result<()> {
        let now = records::now();
        let mut total = std::fs::read_dir(&self.directory)?
            .try_fold(0_u64, |sum, entry| -> Result<u64> {
                Ok(sum + entry?.metadata()?.len())
            })?;
        let mut remove = 0;
        for path in segments.iter() {
            let size = path.metadata()?.len();
            let created = path
                .file_name()
                .and_then(|s| s.to_str())
                .and_then(|s| s.get(7..27))
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            if now.saturating_sub(created) <= RETENTION_SECONDS
                && total + incoming + DATABASE_BYTES <= TOTAL_BYTES
            {
                break;
            }
            std::fs::remove_file(path)?;
            total = total.saturating_sub(size);
            remove += 1;
        }
        segments.drain(..remove);
        if remove > 0 {
            self.conn.execute(
                "UPDATE diagnostic_meta SET value=value+?1 WHERE key='evicted'",
                [remove as i64],
            )?;
        }
        ensure!(
            total + incoming + DATABASE_BYTES <= TOTAL_BYTES,
            "diagnostic_capacity_exhausted"
        );
        Ok(())
    }

    pub(super) fn lock(&self) -> Result<Option<File>> {
        let file = private_open(&self.directory.join("append.lock"), false)?;
        let deadline = Instant::now() + Duration::from_millis(10);
        loop {
            match fs2::FileExt::try_lock_exclusive(&file) {
                Ok(()) => return Ok(Some(file)),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

fn private_open(path: &Path, append: bool) -> Result<File> {
    Ok(OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .append(append)
        .mode(0o600)
        .open(path)?)
}
