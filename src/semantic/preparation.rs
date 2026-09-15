//! Background semantic preparation for one committed source generation.
//!
//! Preparation reads immutable region text in small snapshots, then releases
//! the index read transaction before serialized inference. Progress and
//! outcomes survive process restarts and are keyed by content, never region IDs.

mod cache;
mod worker;

pub use worker::{ClosureWorker, EmbeddingWorker, ForegroundWorker, SharedWorker};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[cfg(not(feature = "semantic"))]
use crate::semantic::{INPUT_VERSION, MODEL_REVISION};

pub const SNAPSHOT_WINDOW: usize = 256;
pub const MAX_BATCH: usize = 8;
pub const MAX_MODEL_TOKENS: usize = 4_096;
/// Maximum padded token count for one preparation batch.
pub const MAX_PADDED_TOKENS: usize = 8_192;
const DEFAULT_WAIT: Duration = Duration::from_secs(60 * 60);
const RETRY_WINDOW: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreparationState {
    Idle,
    Running,
    Completed,
    Superseded,
    Failed,
    Capacity,
}

impl PreparationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Superseded => "superseded",
            Self::Failed => "failed",
            Self::Capacity => "capacity",
        }
    }

    fn terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Superseded | Self::Failed | Self::Capacity
        )
    }
}

impl std::str::FromStr for PreparationState {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "idle" => Ok(Self::Idle),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "superseded" => Ok(Self::Superseded),
            "failed" => Ok(Self::Failed),
            "capacity" => Ok(Self::Capacity),
            _ => Err(format!("unknown preparation state {value:?}")),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreparationStatus {
    pub generation: i64,
    pub state: PreparationState,
    pub cursor: usize,
    pub total: usize,
    pub cached: usize,
    pub missing: usize,
    pub failures: usize,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScheduleReceipt {
    pub captured_generation: i64,
    pub coalesced: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreparationWait {
    pub captured_generation: i64,
    pub state: PreparationState,
    pub status: PreparationStatus,
}

impl PreparationWait {
    pub fn is_terminal(&self) -> bool {
        self.state.terminal()
    }
}

/// Text and its immutable cache identity; no index row identity crosses the worker boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbeddingInput {
    pub content_key: String,
    pub text: String,
    pub token_count: usize,
}

/// Requests preparation in the persistent per-root worker, if one is running.
pub fn schedule(root: &Path, cache: &Path) -> Result<ScheduleReceipt> {
    request_schedule(root, cache)
}

/// Explicitly retries a terminal current-generation preparation run.
pub fn retry(root: &Path, cache: &Path) -> Result<ScheduleReceipt> {
    request_schedule_with_reset(root, cache)
}

/// Returns current-generation counts from the index and verified embedding cache.
pub fn status(root: &Path, cache: &Path) -> Result<PreparationStatus> {
    let mut index = open_index(root, cache)?;
    let mut preparation = cache::PreparationCache::open(cache)?;
    current_status(&mut index, &mut preparation)
}

/// Waits for a captured generation, returning an explicit terminal outcome.
pub fn wait(root: &Path, cache: &Path, captured_generation: i64) -> Result<PreparationWait> {
    wait_timeout(root, cache, captured_generation, DEFAULT_WAIT)
}

/// Bounded variant used by clients that have their own request deadline.
pub fn wait_timeout(
    root: &Path,
    cache: &Path,
    captured_generation: i64,
    timeout: Duration,
) -> Result<PreparationWait> {
    if captured_generation <= 0 {
        bail!("preparation_unavailable: invalid captured source generation");
    }
    let started = Instant::now();
    loop {
        let index = open_index(root, cache)?;
        let current_generation = read_generation(&index)?;
        let preparation = cache::PreparationCache::open(cache)?;
        if current_generation > captured_generation {
            let status = superseded_status(captured_generation, &preparation);
            return Ok(PreparationWait {
                captured_generation,
                state: PreparationState::Superseded,
                status,
            });
        }
        if let Some(run) = preparation.run(captured_generation)? {
            if run.state.terminal() {
                return Ok(wait_from_run(captured_generation, run));
            }
        }
        if current_generation < captured_generation {
            bail!(
                "preparation_unavailable: captured generation {captured_generation} is ahead of index generation {current_generation}"
            );
        }
        if started.elapsed() >= timeout {
            return Ok(timeout_wait(captured_generation));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Runs exactly one current-generation sweep synchronously.
pub fn run_foreground<W: EmbeddingWorker>(
    root: &Path,
    cache: &Path,
    mut worker: W,
) -> Result<PreparationStatus> {
    let mut index = open_index(root, cache)?;
    let generation = read_generation(&index)?;
    if generation <= 0 {
        bail!("preparation_unavailable: no published source generation");
    }
    let Some(_lease) = cache::PreparationLease::try_acquire(cache)? else {
        bail!("preparation_busy: another process owns the preparation sweep");
    };
    let mut preparation = cache::PreparationCache::open(cache)?;
    let _ = prepare_generation(&mut index, &mut preparation, &mut worker, generation, true)?;
    current_status(&mut index, &mut preparation)
}

enum Command {
    Schedule {
        root: PathBuf,
        cache: PathBuf,
        reset_terminal: bool,
    },
    Stop,
}

struct Wake {
    epoch: Mutex<u64>,
    changed: Condvar,
}

/// Owns one persistent preparation thread and its separate SQLite handle.
pub struct PreparationManager {
    commands: mpsc::SyncSender<Command>,
    wake: Arc<Wake>,
    thread: Option<JoinHandle<()>>,
}

impl PreparationManager {
    pub fn new<W: EmbeddingWorker + 'static>(worker: W) -> Self {
        let (sender, receiver) = mpsc::sync_channel(1);
        let wake = Arc::new(Wake {
            epoch: Mutex::new(0),
            changed: Condvar::new(),
        });
        let worker_wake = Arc::clone(&wake);
        let thread = thread::Builder::new()
            .name("trufflepig-semantic-preparation".into())
            .spawn(move || worker_loop(receiver, worker, worker_wake))
            .expect("preparation thread should start");
        Self {
            commands: sender,
            wake,
            thread: Some(thread),
        }
    }

    /// Schedules the current source generation and wakes the persistent worker.
    pub fn schedule(&self, root: &Path, cache: &Path) -> Result<ScheduleReceipt> {
        let receipt = request_schedule(root, cache)?;
        match self.commands.try_send(Command::Schedule {
            root: root.to_owned(),
            cache: cache.to_owned(),
            reset_terminal: false,
        }) {
            Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
            Err(mpsc::TrySendError::Disconnected(_)) => {
                bail!("preparation worker is stopped")
            }
        }
        self.notify();
        Ok(receipt)
    }

    /// Schedules the current generation and reopens a terminal run.
    pub fn retry(&self, root: &Path, cache: &Path) -> Result<ScheduleReceipt> {
        let receipt = request_schedule_with_reset(root, cache)?;
        match self.commands.try_send(Command::Schedule {
            root: root.to_owned(),
            cache: cache.to_owned(),
            reset_terminal: false,
        }) {
            Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
            Err(mpsc::TrySendError::Disconnected(_)) => {
                bail!("preparation worker is stopped")
            }
        }
        self.notify();
        Ok(receipt)
    }

    pub fn status(&self, root: &Path, cache: &Path) -> Result<PreparationStatus> {
        status(root, cache)
    }

    pub fn wait(
        &self,
        root: &Path,
        cache: &Path,
        captured_generation: i64,
    ) -> Result<PreparationWait> {
        self.wait_timeout(root, cache, captured_generation, DEFAULT_WAIT)
    }

    pub fn wait_timeout(
        &self,
        root: &Path,
        cache: &Path,
        captured_generation: i64,
        timeout: Duration,
    ) -> Result<PreparationWait> {
        // A manager-owned worker may not have reached its first command yet;
        // polling also observes progress made by another process holding lease.
        wait_timeout(root, cache, captured_generation, timeout)
    }

    fn notify(&self) {
        if let Ok(mut epoch) = self.wake.epoch.lock() {
            *epoch = epoch.wrapping_add(1);
            self.wake.changed.notify_all();
        }
    }
}

impl Drop for PreparationManager {
    fn drop(&mut self) {
        let _ = self.commands.try_send(Command::Stop);
        self.notify();
        // Providers cannot be force-cancelled by this owner. Dropping the
        // handle keeps daemon shutdown bounded; the worker exits at a command
        // boundary after its provider deadline.
        let _ = self.thread.take();
    }
}

struct WorkerContext {
    root: PathBuf,
    cache: PathBuf,
    _lease: cache::PreparationLease,
    index: Connection,
    preparation: cache::PreparationCache,
}

fn worker_loop<W: EmbeddingWorker>(
    receiver: mpsc::Receiver<Command>,
    mut worker: W,
    wake: Arc<Wake>,
) {
    let mut context: Option<WorkerContext> = None;
    while let Ok(command) = receiver.recv() {
        match command {
            Command::Stop => return,
            Command::Schedule {
                root,
                cache,
                reset_terminal,
            } => {
                let same_context = context
                    .as_ref()
                    .is_some_and(|context| context.root == root && context.cache == cache);
                if !same_context {
                    context = match cache::PreparationLease::try_acquire(&cache) {
                        Ok(Some(lease)) => {
                            match (
                                open_index(&root, &cache),
                                cache::PreparationCache::open(&cache),
                            ) {
                                (Ok(index), Ok(preparation)) => Some(WorkerContext {
                                    root: root.clone(),
                                    cache: cache.clone(),
                                    _lease: lease,
                                    index,
                                    preparation,
                                }),
                                (Err(error), _) | (_, Err(error)) => {
                                    record_worker_error(&cache, error.to_string());
                                    None
                                }
                            }
                        }
                        Ok(None) => None,
                        Err(error) => {
                            record_worker_error(&cache, error.to_string());
                            None
                        }
                    };
                }
                // The worker owns this connection for its lifetime. A failed
                // sweep remains persisted and is visible through status/wait.
                if let Some(context) = context.as_mut()
                    && let Err(error) = process_pending_context(
                        &mut context.index,
                        &mut context.preparation,
                        &mut worker,
                        reset_terminal,
                    )
                {
                    record_worker_error_in_cache(&mut context.preparation, error.to_string());
                }
                if let Ok(mut epoch) = wake.epoch.lock() {
                    *epoch = epoch.wrapping_add(1);
                    wake.changed.notify_all();
                }
            }
        }
    }
}

fn request_schedule(root: &Path, cache: &Path) -> Result<ScheduleReceipt> {
    request_schedule_with_reset_inner(root, cache, false)
}

fn request_schedule_with_reset(root: &Path, cache: &Path) -> Result<ScheduleReceipt> {
    request_schedule_with_reset_inner(root, cache, true)
}

fn request_schedule_with_reset_inner(
    root: &Path,
    cache: &Path,
    reset_terminal: bool,
) -> Result<ScheduleReceipt> {
    let index = open_index(root, cache)?;
    let generation = read_generation(&index)?;
    if generation <= 0 {
        bail!("preparation_unavailable: no published source generation");
    }
    let mut preparation = cache::PreparationCache::open(cache)?;
    let requested = preparation.request(generation)?;
    let reset = reset_terminal && preparation.reset_terminal(generation)?;
    Ok(ScheduleReceipt {
        captured_generation: generation,
        coalesced: requested && !reset,
    })
}

fn process_pending_context<W: EmbeddingWorker>(
    index: &mut Connection,
    preparation: &mut cache::PreparationCache,
    worker: &mut W,
    reset_terminal: bool,
) -> Result<()> {
    let mut reset_terminal = reset_terminal;
    loop {
        let current_generation = read_generation(index)?;
        if current_generation <= 0 {
            return Ok(());
        }
        preparation.supersede_older(current_generation)?;
        let requested = preparation.requested_generation()?.max(current_generation);
        let generation = requested.min(current_generation);
        let Some(run) = prepare_generation(index, preparation, worker, generation, reset_terminal)?
        else {
            return Ok(());
        };
        if run.state == PreparationState::Superseded {
            reset_terminal = false;
            continue;
        }
        // A terminal run is the one sweep for this committed generation.
        return Ok(());
    }
}

fn prepare_generation<W: EmbeddingWorker>(
    index: &mut Connection,
    preparation: &mut cache::PreparationCache,
    worker: &mut W,
    generation: i64,
    reset_terminal: bool,
) -> Result<Option<cache::RunRecord>> {
    let (observed, total) = snapshot_size(index, generation)?;
    if observed != generation {
        preparation.supersede_generation(
            generation,
            total,
            "source generation changed before preparation started",
        )?;
        return Ok(preparation.run(generation)?);
    }
    let existing = preparation.run(generation)?;
    if reset_terminal || existing.as_ref().is_none_or(|run| !run.state.terminal()) {
        let (keys_observed, current_keys) = snapshot_content_keys(index, generation, total)?;
        if keys_observed != generation {
            preparation.supersede_generation(
                generation,
                total,
                "source generation changed before preparation started",
            )?;
            return Ok(preparation.run(generation)?);
        }
        preparation.prune_completions(current_keys.iter())?;
    }
    let run = preparation.begin(generation, total, reset_terminal)?;
    if run.state.terminal() {
        return Ok(Some(run));
    }
    let mut cursor = run.cursor.min(total);
    let mut cached = run.cached;
    let mut missing = run.missing;
    let mut failures = run.failures;
    while cursor < total {
        if read_generation(index)? != generation {
            preparation.finish(
                generation,
                PreparationState::Superseded,
                cursor,
                total,
                cached,
                missing,
                failures,
                Some("source generation superseded during preparation"),
            )?;
            return Ok(preparation.run(generation)?);
        }
        let (observed, units) = snapshot_units(index, generation, cursor)?;
        if observed != generation {
            preparation.finish(
                generation,
                PreparationState::Superseded,
                cursor,
                total,
                cached,
                missing,
                failures,
                Some("source generation superseded during snapshot"),
            )?;
            return Ok(preparation.run(generation)?);
        }
        if units.is_empty() {
            break;
        }
        let pending = pending_inputs(preparation, &units)?;
        let retry_started = Instant::now();
        let mut retry_delay = Duration::from_millis(50);
        loop {
            let inference = worker::run_batches_grouped(worker, &pending, |batch, results| {
                let mut completed = Vec::with_capacity(results.len());
                for (input, result) in batch.iter().zip(results) {
                    match result {
                        Ok(embedding) => completed.push((input.content_key.clone(), embedding)),
                        Err(error) => preparation.fail(&input.content_key, &error.to_string())?,
                    }
                }
                preparation.complete_many(completed.into_iter())
            });
            match inference {
                Ok(()) => break,
                Err(error) if worker.is_retryable_batch_error(&error) => {
                    if retry_started.elapsed() >= RETRY_WINDOW {
                        preparation.finish(
                            generation,
                            PreparationState::Failed,
                            cursor,
                            total,
                            cached,
                            missing,
                            failures,
                            Some("preparation_worker_timeout: retry window exhausted"),
                        )?;
                        return Ok(preparation.run(generation)?);
                    }
                    thread::sleep(retry_delay);
                    retry_delay = (retry_delay * 2).min(Duration::from_secs(1));
                }
                Err(error) if worker.batch_error_is_unit_failure(&error) => {
                    for input in &pending {
                        preparation.fail(&input.content_key, &error.to_string())?;
                    }
                    break;
                }
                Err(error) => {
                    preparation.finish(
                        generation,
                        PreparationState::Failed,
                        cursor,
                        total,
                        cached,
                        missing,
                        failures,
                        Some(&error.to_string()),
                    )?;
                    return Ok(preparation.run(generation)?);
                }
            }
        }
        let cache_snapshot = preparation.snapshot()?;
        let (window_cached, window_missing, window_failures) =
            classify_units(preparation, &units, &cache_snapshot)?;
        cached = cached.saturating_add(window_cached);
        missing = missing.saturating_add(window_missing);
        failures = failures.saturating_add(window_failures);
        cursor = cursor.saturating_add(units.len());
        preparation.progress(generation, cursor, total, cached, missing, failures)?;
    }
    let (observed, _, final_cached, final_missing, final_failures) =
        current_counts(index, preparation, generation)?;
    if observed != generation {
        preparation.finish(
            generation,
            PreparationState::Superseded,
            cursor,
            total,
            cached,
            missing,
            failures,
            Some("source generation superseded after inference"),
        )?;
        return Ok(preparation.run(generation)?);
    }
    cached = final_cached;
    missing = final_missing;
    failures = final_failures;
    let state = if failures > 0 {
        PreparationState::Failed
    } else if missing > 0 {
        // A finite sweep that cannot retain its vectors must terminate rather
        // than evicting and recomputing forever.
        PreparationState::Capacity
    } else {
        PreparationState::Completed
    };
    let error = match state {
        PreparationState::Capacity => Some("embedding cache capacity cannot retain all content"),
        PreparationState::Failed => Some("one or more content units failed inference"),
        _ => None,
    };
    preparation.finish(
        generation, state, cursor, total, cached, missing, failures, error,
    )?;
    Ok(preparation.run(generation)?)
}

fn pending_inputs(
    preparation: &cache::PreparationCache,
    units: &[EmbeddingInput],
) -> Result<Vec<EmbeddingInput>> {
    let cache_snapshot = preparation.snapshot()?;
    let mut pending = Vec::with_capacity(units.len());
    let mut seen = HashMap::with_capacity(units.len());
    for unit in units {
        if preparation.is_cached_in(&unit.content_key, &cache_snapshot)?
            || preparation
                .completion(&unit.content_key)?
                .is_some_and(|done| !done)
        {
            continue;
        }
        if seen.insert(unit.content_key.clone(), ()).is_none() {
            pending.push(unit.clone());
        }
    }
    Ok(pending)
}

fn classify_units(
    preparation: &cache::PreparationCache,
    units: &[EmbeddingInput],
    cache_snapshot: &cache::CachedSnapshot,
) -> Result<(usize, usize, usize)> {
    let mut cached = 0;
    let mut missing = 0;
    let mut failures = 0;
    for unit in units {
        if preparation.is_cached_in(&unit.content_key, cache_snapshot)? {
            cached += 1;
        } else if preparation
            .completion(&unit.content_key)?
            .is_some_and(|done| !done)
        {
            failures += 1;
        } else {
            missing += 1;
        }
    }
    Ok((cached, missing, failures))
}

fn current_status(
    index: &mut Connection,
    preparation: &mut cache::PreparationCache,
) -> Result<PreparationStatus> {
    let generation = read_generation(index)?;
    if generation <= 0 {
        return Ok(PreparationStatus {
            generation,
            state: PreparationState::Idle,
            cursor: 0,
            total: 0,
            cached: 0,
            missing: 0,
            failures: 0,
            error: None,
        });
    }
    let (observed, total, cached, missing, failures) =
        current_counts(index, preparation, generation)?;
    if observed != generation {
        return current_status(index, preparation);
    }
    preparation.counts_for_status(generation, total, cached, missing, failures)
}

fn current_counts(
    index: &mut Connection,
    preparation: &cache::PreparationCache,
    generation: i64,
) -> Result<(i64, usize, usize, usize, usize)> {
    let mut offset = 0;
    let mut total = 0;
    let mut cached = 0;
    let mut missing = 0;
    let mut failures = 0;
    loop {
        let (observed, units) = snapshot_units(index, generation, offset)?;
        if observed != generation {
            return Ok((observed, 0, 0, 0, 0));
        }
        if units.is_empty() {
            break;
        }
        total += units.len();
        let cache_snapshot = preparation.snapshot()?;
        let (window_cached, window_missing, window_failures) =
            classify_units(preparation, &units, &cache_snapshot)?;
        cached += window_cached;
        missing += window_missing;
        failures += window_failures;
        offset += units.len();
    }
    Ok((generation, total, cached, missing, failures))
}

fn snapshot_size(index: &mut Connection, expected: i64) -> Result<(i64, usize)> {
    let transaction = index.unchecked_transaction()?;
    let observed = read_generation(&transaction)?;
    let total: i64 = transaction.query_row(
        "SELECT count(*) FROM regions r JOIN files f ON f.id=r.file_id WHERE f.revision IS NOT NULL",
        [],
        |row| row.get(0),
    )?;
    transaction.commit()?;
    if observed != expected {
        return Ok((observed, 0));
    }
    Ok((observed, total.try_into().context("invalid region count")?))
}

fn snapshot_units(
    index: &mut Connection,
    expected: i64,
    offset: usize,
) -> Result<(i64, Vec<EmbeddingInput>)> {
    let transaction = index.unchecked_transaction()?;
    let observed = read_generation(&transaction)?;
    let mut units = Vec::with_capacity(SNAPSHOT_WINDOW);
    if observed == expected {
        let mut statement = transaction.prepare(
            "SELECT r.body FROM regions r JOIN files f ON f.id=r.file_id
             WHERE f.revision IS NOT NULL ORDER BY f.path,r.start,r.end,r.id
             LIMIT ?1 OFFSET ?2",
        )?;
        let rows = statement.query_map(params![SNAPSHOT_WINDOW as i64, offset as i64], |row| {
            row.get::<_, String>(0)
        })?;
        for row in rows {
            let text = row?;
            units.push(EmbeddingInput {
                content_key: content_key(&text),
                token_count: estimate_tokens(&text),
                text,
            });
        }
    }
    transaction.commit()?;
    Ok((observed, units))
}

fn snapshot_content_keys(
    index: &mut Connection,
    expected: i64,
    total: usize,
) -> Result<(i64, HashSet<String>)> {
    let mut offset = 0;
    let mut keys = HashSet::with_capacity(total);
    loop {
        let (observed, units) = snapshot_units(index, expected, offset)?;
        if observed != expected {
            return Ok((observed, keys));
        }
        if units.is_empty() {
            break;
        }
        let count = units.len();
        keys.extend(units.into_iter().map(|unit| unit.content_key));
        offset += count;
    }
    Ok((expected, keys))
}

fn open_index(root: &Path, cache: &Path) -> Result<Connection> {
    let root = root
        .canonicalize()
        .context("canonicalize preparation repository root")?;
    if !root.is_dir() {
        bail!("preparation_unavailable: repository root is not a directory");
    }
    let cache = cache.canonicalize().or_else(|_| {
        std::fs::create_dir_all(cache)?;
        cache.canonicalize()
    })?;
    if root.starts_with(&cache) {
        bail!("preparation_unavailable: cache must not contain repository root");
    }
    let connection = Connection::open(cache.join("index.sqlite3"))?;
    connection.busy_timeout(Duration::from_secs(10))?;
    let stored_root: Option<String> = connection
        .query_row("SELECT value FROM meta WHERE key='root'", [], |row| {
            row.get(0)
        })
        .optional()?;
    let expected_root = crate::store::encode_path(&root);
    if stored_root.as_deref() != Some(expected_root.as_str()) {
        bail!("preparation_cache_mismatch: cache belongs to another repository root");
    }
    Ok(connection)
}

fn read_generation(connection: &Connection) -> Result<i64> {
    Ok(connection
        .query_row("SELECT value FROM meta WHERE key='generation'", [], |row| {
            row.get::<_, String>(0)
        })?
        .parse()?)
}

#[cfg(feature = "semantic")]
fn content_key(text: &str) -> String {
    crate::semantic::embedding_cache::content_key(text)
}

#[cfg(not(feature = "semantic"))]
fn content_key(text: &str) -> String {
    use sha2::{Digest, Sha256};
    const MODEL_MANIFEST: &[u8] = include_bytes!("../../evaluation/semantic_gate/model.json");
    const NORMALIZATION: &[u8] = b"l2";
    let dimensions = crate::semantic::DIMENSIONS.to_le_bytes();
    let parts: [(&[u8], &[u8]); 6] = [
        (b"manifest", MODEL_MANIFEST),
        (b"model_revision", MODEL_REVISION.as_bytes()),
        (b"input_version", INPUT_VERSION.as_bytes()),
        (b"dimensions", &dimensions),
        (b"normalization", NORMALIZATION),
        (b"content", text.as_bytes()),
    ];
    let mut digest = Sha256::new();
    for (label, value) in parts {
        digest.update((label.len() as u64).to_le_bytes());
        digest.update(label);
        digest.update((value.len() as u64).to_le_bytes());
        digest.update(value);
    }
    format!("{:x}", digest.finalize())
}

fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4).max(1)
}

fn wait_from_run(generation: i64, run: cache::RunRecord) -> PreparationWait {
    let status = PreparationStatus {
        generation,
        state: run.state,
        cursor: run.cursor,
        total: run.total,
        cached: run.cached,
        missing: run.missing,
        failures: run.failures,
        error: run.error,
    };
    PreparationWait {
        captured_generation: generation,
        state: status.state,
        status,
    }
}

fn superseded_status(generation: i64, preparation: &cache::PreparationCache) -> PreparationStatus {
    let run = preparation.run(generation).ok().flatten();
    let (cursor, total, cached, missing, failures, error) = run
        .map(|run| {
            (
                run.cursor,
                run.total,
                run.cached,
                run.missing,
                run.failures,
                run.error,
            )
        })
        .unwrap_or_default();
    PreparationStatus {
        generation,
        state: PreparationState::Superseded,
        cursor,
        total,
        cached,
        missing,
        failures,
        error,
    }
}

fn timeout_wait(generation: i64) -> PreparationWait {
    let status = PreparationStatus {
        generation,
        state: PreparationState::Failed,
        cursor: 0,
        total: 0,
        cached: 0,
        missing: 0,
        failures: 0,
        error: Some("preparation_wait_timeout".into()),
    };
    PreparationWait {
        captured_generation: generation,
        state: PreparationState::Failed,
        status,
    }
}

fn record_worker_error_in_cache(preparation: &mut cache::PreparationCache, message: String) {
    let Ok(requested) = preparation.requested_generation() else {
        return;
    };
    if requested > 0 {
        let _ = preparation.record_worker_error(requested, &message);
    }
}

fn record_worker_error(cache: &Path, message: String) {
    let Ok(mut preparation) = cache::PreparationCache::open(cache) else {
        return;
    };
    record_worker_error_in_cache(&mut preparation, message);
}

#[cfg(test)]
mod tests;
