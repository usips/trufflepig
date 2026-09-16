//! Seeds a linked worktree's cache with its member's content-keyed extraction
//! facts and embeddings so the first reconcile skips parsing unchanged files.
//! A seeded store has generation 0 and no publication, so the worktree always
//! publishes its own generation 1; `preparation.sqlite3` is never copied.

use super::{encode_path, publish, schema};
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::PermissionsExt,
    path::Path,
    time::Duration,
};

/// Source indexes above this size are not worth copying at request time.
pub const MAX_SEED_SOURCE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const INDEX_NAME: &str = "index.sqlite3";
const EMBEDDINGS_NAME: &str = "embeddings.sqlite";
const SEED_SUFFIX: &str = ".seed";

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

/// Never fails: any error removes partial files, logs once to stderr, and yields `Skipped`.
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
    match seed(
        member_cache,
        worktree_root,
        worktree_cache,
        max_source_bytes,
    ) {
        Ok(outcome) => {
            if let SeedOutcome::Seeded {
                extraction_rows,
                embeddings,
            } = outcome
            {
                eprintln!(
                    "trufflepig: seeded {} from {} ({extraction_rows} extraction rows, embeddings: {})",
                    worktree_cache.display(),
                    member_cache.display(),
                    if embeddings { "yes" } else { "no" }
                );
            }
            outcome
        }
        Err(error) => {
            remove_partial_files(worktree_cache);
            eprintln!("trufflepig: seed skipped: {error:#}");
            SeedOutcome::Skipped("seed failed")
        }
    }
}

fn seed(
    member_cache: &Path,
    worktree_root: &Path,
    worktree_cache: &Path,
    max_source_bytes: u64,
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
    let _writer_lock = lock_index_writer(worktree_cache)?;
    if destination.exists() {
        return Ok(SeedOutcome::Present);
    }
    let source_bytes = file_size(&source)? + file_size(&member_cache.join("index.sqlite3-wal"))?;
    if source_bytes > max_source_bytes {
        return Ok(SeedOutcome::Skipped("source index exceeds seed limit"));
    }
    if fs2::available_space(worktree_cache)? < source_bytes.saturating_mul(2) {
        return Ok(SeedOutcome::Skipped("insufficient free space"));
    }
    let root = worktree_root
        .canonicalize()
        .context("canonicalize worktree root")?;
    let extraction_rows = copy_extraction_facts(&source, &root, worktree_cache)?;
    let embeddings = match copy_embeddings(member_cache, worktree_cache) {
        Ok(copied) => copied,
        Err(error) => {
            let _ = fs::remove_file(seed_path(worktree_cache, EMBEDDINGS_NAME));
            eprintln!("trufflepig: seed embeddings skipped: {error:#}");
            false
        }
    };
    Ok(SeedOutcome::Seeded {
        extraction_rows,
        embeddings,
    })
}

/// Same lease as `Store::index`, so a racing daemon's first scan waits for the seed.
fn lock_index_writer(worktree_cache: &Path) -> Result<File> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(worktree_cache.join("index.lock"))?;
    fs2::FileExt::lock_exclusive(&lock)?;
    Ok(lock)
}

/// Builds `index.sqlite3.seed` holding only meta and the member's extraction
/// facts, then renames it into place.
fn copy_extraction_facts(source: &Path, root: &Path, worktree_cache: &Path) -> Result<u64> {
    let staged = seed_path(worktree_cache, INDEX_NAME);
    remove_stale_seed(&staged)?;
    let mut conn = Connection::open(&staged)?;
    conn.busy_timeout(Duration::from_secs(10))?;
    conn.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;")?;
    schema::create(&conn)?;
    publish::create_journal(&conn)?;
    conn.execute("ATTACH DATABASE ?1 AS source", [utf8_path(source)?])?;
    let copied: Result<u64> = (|| {
        let transaction = conn.transaction()?;
        transaction.execute(
            "INSERT INTO meta(key,value) VALUES('index_epoch',?1)",
            [uuid::Uuid::new_v4().to_string()],
        )?;
        transaction.execute(
            "INSERT INTO meta(key,value) VALUES('root',?1)",
            [encode_path(root)],
        )?;
        let rows = transaction.execute(
            "INSERT INTO extraction_cache SELECT * FROM source.extraction_cache",
            [],
        )?;
        transaction.commit()?;
        Ok(rows as u64)
    })();
    let detach = conn.execute_batch("DETACH DATABASE source");
    let rows = copied?;
    detach?;
    drop(conn);
    fs::rename(&staged, worktree_cache.join(INDEX_NAME))?;
    Ok(rows)
}

/// Copies the member's embeddings when the worktree has none; the cache is
/// content-keyed and `VACUUM INTO` keeps `user_version`, so the copy is valid.
fn copy_embeddings(member_cache: &Path, worktree_cache: &Path) -> Result<bool> {
    let source = member_cache.join(EMBEDDINGS_NAME);
    let destination = worktree_cache.join(EMBEDDINGS_NAME);
    if !source.is_file() || destination.exists() {
        return Ok(false);
    }
    let staged = seed_path(worktree_cache, EMBEDDINGS_NAME);
    remove_stale_seed(&staged)?;
    let conn = Connection::open(&source)?;
    conn.busy_timeout(Duration::from_secs(10))?;
    conn.execute("VACUUM INTO ?1", [utf8_path(&staged)?])?;
    drop(conn);
    fs::rename(&staged, &destination)?;
    Ok(true)
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
        let staged = seed_path(worktree_cache, name);
        let _ = fs::remove_file(journal_path(&staged));
        let _ = fs::remove_file(staged);
    }
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
