//! Seeds a linked worktree's cache with its member's content-keyed extraction
//! facts and embeddings so the first reconcile skips parsing unchanged files.
//! A seeded store has generation 0 and no publication, so the worktree always
//! publishes its own generation 1; `preparation.sqlite3` is never copied.
//! Seeding never writes to stdout or stderr: each attempt that does work is
//! recorded in `<worktree cache>/seed-outcome.json` ([`read_seed_outcome`]).

use super::{encode_path, publish, schema};
use crate::daemon::deadline::QueryDeadline;
use anyhow::{Context, Result};
use rusqlite::{Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Read},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
    time::Duration,
};

/// Source indexes above this size are not worth copying at request time.
pub const MAX_SEED_SOURCE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const INDEX_NAME: &str = "index.sqlite3";
const EMBEDDINGS_NAME: &str = "embeddings.sqlite";
const SEED_SUFFIX: &str = ".seed";
const OUTCOME_NAME: &str = "seed-outcome.json";
const SEED_DEADLINE: Duration = Duration::from_secs(2);
const SKIPPED_SEED_MEMO_TTL: Duration = Duration::from_secs(5);
const MAX_SEED_OUTCOME_BYTES: u64 = 16 * 1024;
const SEED_MEMO_VERSION: u8 = 1;
const SQLITE_PROGRESS_STEPS: i32 = 1_000;

/// Result of seeding a worktree cache; `Skipped` names the reason for diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SeedOutcome {
    Present,
    Seeded {
        extraction_rows: u64,
        embeddings: bool,
    },
    Skipped(&'static str),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct SeedMemoIdentity {
    member_cache: String,
    worktree_root: String,
    worktree_cache: String,
    source_stamp: SeedSourceStamp,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct SeedSourceStamp {
    index: Option<SeedFileStamp>,
    index_wal: Option<SeedFileStamp>,
    index_journal: Option<SeedFileStamp>,
    embeddings: Option<SeedFileStamp>,
    embeddings_wal: Option<SeedFileStamp>,
    embeddings_journal: Option<SeedFileStamp>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct SeedFileStamp {
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[derive(Debug, Deserialize, Serialize)]
struct SkippedSeedMemo {
    version: u8,
    created_ms: u64,
    expires_ms: u64,
    identity: SeedMemoIdentity,
}

/// Never fails: errors yield `Skipped`, with partial files cleaned under the lease.
pub fn ensure_seeded(
    member_cache: &Path,
    worktree_root: &Path,
    worktree_cache: &Path,
) -> SeedOutcome {
    ensure_seeded_with_limit(
        member_cache,
        worktree_root,
        worktree_cache,
        MAX_SEED_SOURCE_BYTES,
    )
}

/// `ensure_seeded` with an explicit source-size bound, for tests.
pub fn ensure_seeded_with_limit(
    member_cache: &Path,
    worktree_root: &Path,
    worktree_cache: &Path,
    max_source_bytes: u64,
) -> SeedOutcome {
    ensure_seeded_with_deadline(
        member_cache,
        worktree_root,
        worktree_cache,
        max_source_bytes,
        QueryDeadline::after(SEED_DEADLINE),
    )
}

/// Seeds a worktree cache within the caller's original deadline, capped at two seconds.
pub fn ensure_seeded_until(
    member_cache: &Path,
    worktree_root: &Path,
    worktree_cache: &Path,
    deadline: &QueryDeadline,
) -> SeedOutcome {
    ensure_seeded_within(
        member_cache,
        worktree_root,
        worktree_cache,
        deadline,
        deadline,
    )
}

/// Seeds within `slice`, a bound shared with other work, capped at two seconds.
/// A skipped seed is memoized while `caller`, the request deadline, is unexpired,
/// so a slice that ends the copy early does not force every request to repeat it.
pub fn ensure_seeded_within(
    member_cache: &Path,
    worktree_root: &Path,
    worktree_cache: &Path,
    slice: &QueryDeadline,
    caller: &QueryDeadline,
) -> SeedOutcome {
    if slice.expired() {
        return SeedOutcome::Skipped("seed deadline expired");
    }
    let seed_deadline = slice.capped(SEED_DEADLINE);
    if seed_deadline.expired() {
        return SeedOutcome::Skipped("seed deadline expired");
    }
    if worktree_cache.join(INDEX_NAME).exists() {
        return SeedOutcome::Present;
    }

    let identity =
        SeedMemoIdentity::capture(member_cache, worktree_root, worktree_cache, &seed_deadline);
    if seed_deadline.expired() {
        return SeedOutcome::Skipped("seed deadline expired");
    }
    if let Some(identity) = identity.as_ref()
        && let Some(outcome) = read_skipped_seed_memo(worktree_cache, identity, &seed_deadline)
    {
        if !seed_deadline.expired() {
            return outcome;
        }
        return SeedOutcome::Skipped("seed deadline expired");
    }
    if seed_deadline.expired() {
        return SeedOutcome::Skipped("seed deadline expired");
    }

    ensure_seeded_recorded(
        member_cache,
        worktree_root,
        worktree_cache,
        MAX_SEED_SOURCE_BYTES,
        seed_deadline,
        identity.as_ref(),
        Some(caller),
    )
}

fn ensure_seeded_with_deadline(
    member_cache: &Path,
    worktree_root: &Path,
    worktree_cache: &Path,
    max_source_bytes: u64,
    deadline: QueryDeadline,
) -> SeedOutcome {
    ensure_seeded_recorded(
        member_cache,
        worktree_root,
        worktree_cache,
        max_source_bytes,
        deadline,
        None,
        None,
    )
}

fn ensure_seeded_recorded(
    member_cache: &Path,
    worktree_root: &Path,
    worktree_cache: &Path,
    max_source_bytes: u64,
    deadline: QueryDeadline,
    memo_identity: Option<&SeedMemoIdentity>,
    caller_deadline: Option<&QueryDeadline>,
) -> SeedOutcome {
    if deadline.expired() {
        return SeedOutcome::Skipped("seed deadline expired");
    }
    let mut record = json!({"source": encode_path(member_cache), "recorded_ms": now_ms()});
    let outcome = match seed(
        member_cache,
        worktree_root,
        worktree_cache,
        max_source_bytes,
        deadline,
        &mut record,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            record["error"] = format!("{error:#}").into();
            SeedOutcome::Skipped("seed failed")
        }
    };
    match &outcome {
        SeedOutcome::Present => return outcome,
        SeedOutcome::Seeded {
            extraction_rows,
            embeddings,
        } => {
            record["outcome"] = "seeded".into();
            record["extraction_rows"] = (*extraction_rows).into();
            record["embeddings"] = (*embeddings).into();
        }
        SeedOutcome::Skipped(reason) => {
            record["outcome"] = "skipped".into();
            record["reason"] = (*reason).into();
            if let (Some(identity), Some(caller_deadline)) = (memo_identity, caller_deadline)
                && !caller_deadline.expired()
                && let Some(final_identity) = SeedMemoIdentity::capture(
                    member_cache,
                    worktree_root,
                    worktree_cache,
                    caller_deadline,
                )
                && final_identity == *identity
            {
                let created_ms = now_ms();
                let memo = SkippedSeedMemo {
                    version: SEED_MEMO_VERSION,
                    created_ms,
                    expires_ms: created_ms.saturating_add(
                        SKIPPED_SEED_MEMO_TTL.as_millis().min(u64::MAX as u128) as u64,
                    ),
                    identity: final_identity,
                };
                record["memo"] = json!(memo);
            }
        }
    }
    // Best effort: the record is diagnostic, and a missing one only hides detail.
    let _ = fs::write(worktree_cache.join(OUTCOME_NAME), record.to_string());
    outcome
}

impl SeedMemoIdentity {
    fn capture(
        member_cache: &Path,
        worktree_root: &Path,
        worktree_cache: &Path,
        deadline: &QueryDeadline,
    ) -> Option<Self> {
        let member_cache = canonicalize_for_memo(member_cache, deadline)?;
        if deadline.expired() {
            return None;
        }
        let worktree_root = fs::canonicalize(worktree_root).ok()?;
        let worktree_cache = canonicalize_for_memo(worktree_cache, deadline)?;
        let source_stamp = SeedSourceStamp::capture(&member_cache, deadline)?;
        Some(Self {
            member_cache: encode_path(&member_cache),
            worktree_root: encode_path(&worktree_root),
            worktree_cache: encode_path(&worktree_cache),
            source_stamp,
        })
    }
}

impl SeedSourceStamp {
    fn capture(cache: &Path, deadline: &QueryDeadline) -> Option<Self> {
        Some(Self {
            index: seed_file_stamp(&cache.join(INDEX_NAME), deadline)?,
            index_wal: seed_file_stamp(&cache.join("index.sqlite3-wal"), deadline)?,
            index_journal: seed_file_stamp(&cache.join("index.sqlite3-journal"), deadline)?,
            embeddings: seed_file_stamp(&cache.join(EMBEDDINGS_NAME), deadline)?,
            embeddings_wal: seed_file_stamp(&cache.join("embeddings.sqlite-wal"), deadline)?,
            embeddings_journal: seed_file_stamp(
                &cache.join("embeddings.sqlite-journal"),
                deadline,
            )?,
        })
    }
}

fn canonicalize_for_memo(path: &Path, deadline: &QueryDeadline) -> Option<std::path::PathBuf> {
    if deadline.expired() {
        return None;
    }
    match fs::canonicalize(path) {
        Ok(path) => Some(path),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let name = path.file_name()?;
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            Some(canonicalize_for_memo(parent, deadline)?.join(name))
        }
        Err(_) => None,
    }
}

fn seed_file_stamp(path: &Path, deadline: &QueryDeadline) -> Option<Option<SeedFileStamp>> {
    if deadline.expired() {
        return None;
    }
    match fs::metadata(path) {
        Ok(metadata) => Some(Some(SeedFileStamp {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })),
        Err(error) if error.kind() == ErrorKind::NotFound => Some(None),
        Err(_) => None,
    }
}

fn read_skipped_seed_memo(
    worktree_cache: &Path,
    identity: &SeedMemoIdentity,
    deadline: &QueryDeadline,
) -> Option<SeedOutcome> {
    if deadline.expired() {
        return None;
    }
    let file = File::open(worktree_cache.join(OUTCOME_NAME)).ok()?;
    let size = file.metadata().ok()?.len();
    if size > MAX_SEED_OUTCOME_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(MAX_SEED_OUTCOME_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_SEED_OUTCOME_BYTES || deadline.expired() {
        return None;
    }
    let record: Value = serde_json::from_slice(&bytes).ok()?;
    if record.get("outcome")?.as_str()? != "skipped" {
        return None;
    }
    let memo: SkippedSeedMemo = serde_json::from_value(record.get("memo")?.clone()).ok()?;
    let now = now_ms();
    let ttl_ms = SKIPPED_SEED_MEMO_TTL.as_millis().min(u64::MAX as u128) as u64;
    if memo.version != SEED_MEMO_VERSION
        || memo.identity != *identity
        || now < memo.created_ms
        || now > memo.expires_ms
        || memo.expires_ms.saturating_sub(memo.created_ms) > ttl_ms
    {
        return None;
    }
    skipped_outcome(record.get("reason")?.as_str()?)
}

fn skipped_outcome(reason: &str) -> Option<SeedOutcome> {
    Some(SeedOutcome::Skipped(match reason {
        "member cache has no index" => "member cache has no index",
        "index writer lease is held" => "index writer lease is held",
        "source index exceeds seed limit" => "source index exceeds seed limit",
        "insufficient free space" => "insufficient free space",
        "seed failed" => "seed failed",
        _ => return None,
    }))
}

/// The last recorded seeding attempt for `worktree_cache`, if any.
pub fn read_seed_outcome(worktree_cache: &Path) -> Option<Value> {
    let bytes = fs::read(worktree_cache.join(OUTCOME_NAME)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

fn seed(
    member_cache: &Path,
    worktree_root: &Path,
    worktree_cache: &Path,
    max_source_bytes: u64,
    deadline: QueryDeadline,
    record: &mut Value,
) -> Result<SeedOutcome> {
    fs::create_dir_all(worktree_cache)?;
    fs::set_permissions(worktree_cache, fs::Permissions::from_mode(0o700))?;
    let destination = worktree_cache.join(INDEX_NAME);
    if destination.exists() {
        return Ok(SeedOutcome::Present);
    }
    let source = member_cache.join(INDEX_NAME);
    if !source.is_file() {
        return Ok(SeedOutcome::Skipped("member cache has no index"));
    }
    let _writer_lock = match lock_index_writer(worktree_cache) {
        Ok(lock) => lock,
        Err(error) if error.kind() == ErrorKind::WouldBlock => {
            return Ok(SeedOutcome::Skipped("index writer lease is held"));
        }
        Err(error) => return Err(error.into()),
    };
    let staged_embeddings = seed_path(worktree_cache, EMBEDDINGS_NAME);
    let embeddings_destination = worktree_cache.join(EMBEDDINGS_NAME);
    let mut embeddings_published = false;
    let mut index_published = false;
    let result = (|| {
        ensure_seed_deadline(&deadline)?;
        if destination.exists() {
            return Ok(SeedOutcome::Present);
        }
        let source_bytes =
            file_size(&source)? + file_size(&member_cache.join("index.sqlite3-wal"))?;
        if source_bytes > max_source_bytes {
            return Ok(SeedOutcome::Skipped("source index exceeds seed limit"));
        }
        if fs2::available_space(worktree_cache)? < source_bytes.saturating_mul(2) {
            return Ok(SeedOutcome::Skipped("insufficient free space"));
        }
        let root = worktree_root
            .canonicalize()
            .context("canonicalize worktree root")?;
        let extraction_rows = copy_extraction_facts(&source, &root, worktree_cache, &deadline)?;
        ensure_seed_deadline(&deadline)?;
        let embeddings = match copy_embeddings(member_cache, worktree_cache, &deadline) {
            Ok(copied) => copied,
            Err(error) => {
                if deadline.expired() {
                    return Err(error);
                }
                remove_seed_pair(&seed_path(worktree_cache, EMBEDDINGS_NAME));
                record["embeddings_error"] = format!("{error:#}").into();
                false
            }
        };
        ensure_seed_deadline(&deadline)?;
        if embeddings {
            ensure_seed_deadline(&deadline)?;
            fs::rename(&staged_embeddings, &embeddings_destination)?;
            embeddings_published = true;
        }
        ensure_seed_deadline(&deadline)?;
        let staged_index = seed_path(worktree_cache, INDEX_NAME);
        fs::rename(&staged_index, &destination)?;
        index_published = true;
        ensure_seed_deadline(&deadline)?;
        Ok(SeedOutcome::Seeded {
            extraction_rows,
            embeddings,
        })
    })()
    .map_err(|error| deadline.classify(error));
    if result.is_err() {
        if index_published {
            let _ = fs::remove_file(&destination);
        }
        if embeddings_published {
            let _ = fs::remove_file(&embeddings_destination);
        }
        remove_partial_files(worktree_cache);
    }
    result
}

/// Same lease as `Store::index`, so a racing daemon's first scan waits for the seed.
fn lock_index_writer(worktree_cache: &Path) -> std::io::Result<File> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(worktree_cache.join("index.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock)?;
    Ok(lock)
}

/// Builds the staged index from meta and the member's extraction facts.
fn copy_extraction_facts(
    source: &Path,
    root: &Path,
    worktree_cache: &Path,
    deadline: &QueryDeadline,
) -> Result<u64> {
    ensure_seed_deadline(deadline)?;
    let staged = seed_path(worktree_cache, INDEX_NAME);
    remove_stale_seed(&staged)?;
    let mut conn = Connection::open(&staged)?;
    refresh_seed_busy_timeout(&conn, deadline)?;
    let progress_deadline = deadline.clone();
    conn.progress_handler(
        SQLITE_PROGRESS_STEPS,
        Some(move || progress_deadline.expired()),
    )?;
    conn.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;")?;

    let copied: Result<u64> = (|| {
        ensure_seed_deadline(deadline)?;
        refresh_seed_busy_timeout(&conn, deadline).context("before staged index transaction")?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_seed_deadline(deadline)?;
        schema::create(&transaction).context("create staged index schema")?;
        ensure_seed_deadline(deadline)?;
        publish::create_journal(&transaction).context("create staged publication journal")?;
        ensure_seed_deadline(deadline)?;
        transaction
            .execute(
                "INSERT INTO meta(key,value) VALUES('index_epoch',?1)",
                [uuid::Uuid::new_v4().to_string()],
            )
            .context("write staged index epoch")?;
        transaction
            .execute(
                "INSERT INTO meta(key,value) VALUES('root',?1)",
                [encode_path(root)],
            )
            .context("write staged index root")?;
        refresh_seed_busy_timeout(&transaction, deadline)
            .context("before attaching member index")?;
        transaction
            .execute("ATTACH DATABASE ?1 AS source", [utf8_path(source)?])
            .context("attach member extraction index")?;
        refresh_seed_busy_timeout(&transaction, deadline)
            .context("before copying member extraction facts")?;
        let rows = transaction
            .execute(
                "INSERT INTO extraction_cache SELECT * FROM source.extraction_cache",
                [],
            )
            .context("copy member extraction facts")?;
        refresh_seed_busy_timeout(&transaction, deadline)
            .context("before committing staged index")?;
        transaction
            .commit()
            .context("commit staged extraction facts")?;
        Ok(rows as u64)
    })();
    let detach = conn.execute_batch("DETACH DATABASE source");
    let rows = copied?;
    detach?;
    drop(conn);
    ensure_seed_deadline(deadline)?;
    Ok(rows)
}

fn refresh_seed_busy_timeout(conn: &Connection, deadline: &QueryDeadline) -> Result<()> {
    ensure_seed_deadline(deadline)?;
    conn.busy_timeout(deadline.cap(SEED_DEADLINE))?;
    ensure_seed_deadline(deadline)
}

/// Copies the member's embeddings when the worktree has none; the cache is
/// content-keyed and `VACUUM INTO` keeps `user_version`, so the copy is valid.
fn copy_embeddings(
    member_cache: &Path,
    worktree_cache: &Path,
    deadline: &QueryDeadline,
) -> Result<bool> {
    ensure_seed_deadline(deadline)?;
    let source = member_cache.join(EMBEDDINGS_NAME);
    let destination = worktree_cache.join(EMBEDDINGS_NAME);
    if !source.is_file() || destination.exists() {
        return Ok(false);
    }
    let staged = seed_path(worktree_cache, EMBEDDINGS_NAME);
    remove_stale_seed(&staged)?;
    let conn = Connection::open(&source)?;
    conn.busy_timeout(deadline.cap(SEED_DEADLINE))?;
    let progress_deadline = deadline.clone();
    conn.progress_handler(
        SQLITE_PROGRESS_STEPS,
        Some(move || progress_deadline.expired()),
    )?;
    conn.execute("VACUUM INTO ?1", [utf8_path(&staged)?])?;
    drop(conn);
    ensure_seed_deadline(deadline)?;
    Ok(true)
}

fn ensure_seed_deadline(deadline: &QueryDeadline) -> Result<()> {
    anyhow::ensure!(!deadline.expired(), "seed deadline expired");
    Ok(())
}

fn seed_path(worktree_cache: &Path, name: &str) -> std::path::PathBuf {
    worktree_cache.join(format!("{name}{SEED_SUFFIX}"))
}

fn remove_stale_seed(staged: &Path) -> Result<()> {
    for path in [staged.to_owned(), journal_path(staged)] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("remove stale seed file"),
        }
    }
    Ok(())
}

fn journal_path(staged: &Path) -> std::path::PathBuf {
    let mut journal = staged.as_os_str().to_owned();
    journal.push("-journal");
    journal.into()
}

fn remove_partial_files(worktree_cache: &Path) {
    for name in [INDEX_NAME, EMBEDDINGS_NAME] {
        remove_seed_pair(&seed_path(worktree_cache, name));
    }
}

fn remove_seed_pair(staged: &Path) {
    let _ = fs::remove_file(journal_path(staged));
    let _ = fs::remove_file(staged);
}

fn file_size(path: &Path) -> Result<u64> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error).context("measure seed source"),
    }
}

fn utf8_path(path: &Path) -> Result<&str> {
    path.to_str().context("non-UTF8 cache path")
}

#[cfg(test)]
mod seed_deadline_tests;
#[cfg(test)]
mod seed_memo_tests;
