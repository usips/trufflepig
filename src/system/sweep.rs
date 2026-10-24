//! Evicts default-base root caches whose recorded root was deleted and removes
//! stale `stage-*` directories left by killed indexers. Only directories holding
//! `index.sqlite3` or a [`CACHE_ROOT_MARKER`] are eviction candidates, a grace
//! period protects caches being created or written, and a root that may sit on an
//! unmounted volume is kept ([`vanished_root`]). Stage removal runs only while the
//! cache's writer lease is free (`crate::store::sweep_stale_stages`).

mod vanished_root;

use fs2::FileExt;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::{
    fs::{self, Metadata, OpenOptions},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

/// Time between router sweeps; the first runs at startup.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(600);
/// Caches modified within this window are never evicted.
pub const SWEEP_GRACE: Duration = Duration::from_secs(60);
const LOCK_WAIT: Duration = Duration::from_secs(3);
/// Names the root of a cache that may hold no index yet (a worktree cache whose
/// queries were answered through its parent's index); holds the encoded root.
pub const CACHE_ROOT_MARKER: &str = "cache-root";

/// Records `root` in `cache` so the sweep can evict it before any index exists.
pub fn record_cache_root(cache: &Path, root: &Path) {
    let marker = cache.join(CACHE_ROOT_MARKER);
    if !marker.exists() {
        let _ = fs::write(marker, crate::store::encode_path(root));
    }
}

/// One removed cache directory and the root it served.
#[derive(Clone, Debug, Serialize)]
pub struct Evicted {
    pub cache: PathBuf,
    pub root: PathBuf,
    pub daemon_stopped: bool,
}

/// What one sweep removed: whole caches and stale stage directories in kept caches.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SweepReport {
    pub evicted: Vec<Evicted>,
    pub removed_stages: Vec<PathBuf>,
}

/// Pure over `base`; never errors on one entry.
pub fn sweep(base: &Path, now: SystemTime, grace: Duration) -> SweepReport {
    let mut report = SweepReport::default();
    let Ok(entries) = fs::read_dir(base) else {
        return report;
    };
    for entry in entries.flatten() {
        let cache = entry.path();
        if !fs::metadata(&cache).is_ok_and(|metadata| metadata.is_dir()) {
            continue;
        }
        match evict_entry(base, &cache, now, grace) {
            Some(evicted) => report.evicted.push(evicted),
            None => report.removed_stages.extend(remove_stale_stages(&cache)),
        }
    }
    report
}

fn evict_entry(base: &Path, cache: &Path, now: SystemTime, grace: Duration) -> Option<Evicted> {
    let directory = fs::metadata(cache).ok().filter(Metadata::is_dir)?;
    let index_path = cache.join("index.sqlite3");
    let marker_path = cache.join(CACHE_ROOT_MARKER);
    let indexed = fs::metadata(&index_path).ok().filter(Metadata::is_file);
    let record = match &indexed {
        Some(index) => index.clone(),
        None => fs::metadata(&marker_path).ok().filter(Metadata::is_file)?,
    };
    if modified_within(&directory, now, grace) || modified_within(&record, now, grace) {
        return None;
    }
    let root = match indexed {
        Some(_) => recorded_root(&index_path)?,
        None => crate::store::decode_path(fs::read_to_string(&marker_path).ok()?.trim()).ok()?,
    };
    if !vanished_root::root_deleted(&root, base) {
        return None;
    }
    let daemon_stopped = cache.join("daemon.sock").exists()
        && crate::daemon::stop(cache).is_ok_and(|reply| reply.contains("\"stopped\""));
    let _lock = acquire_daemon_lock(cache)?;
    fs::remove_dir_all(cache).ok()?;
    eprintln!(
        "trufflepig: evicted cache {} for vanished root {}",
        cache.display(),
        root.display()
    );
    Some(Evicted {
        cache: cache.to_owned(),
        root,
        daemon_stopped,
    })
}

fn remove_stale_stages(cache: &Path) -> Vec<PathBuf> {
    match crate::store::sweep_stale_stages(cache) {
        Ok(Some(removed)) => {
            for stage in &removed {
                eprintln!("trufflepig: removed stale stage {}", stage.display());
            }
            removed
        }
        Ok(None) => Vec::new(),
        Err(error) => {
            eprintln!(
                "trufflepig: stale stage cleanup failed in {}: {error:#}",
                cache.display()
            );
            Vec::new()
        }
    }
}

/// A missing or future modification time counts as recent.
fn modified_within(metadata: &Metadata, now: SystemTime, grace: Duration) -> bool {
    metadata.modified().ok().is_none_or(|modified| {
        now.duration_since(modified).is_ok_and(|age| age < grace) || now < modified
    })
}

fn recorded_root(index: &Path) -> Option<PathBuf> {
    let conn = Connection::open_with_flags(
        index,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    conn.busy_timeout(Duration::from_secs(1)).ok()?;
    let encoded: String = conn
        .query_row("SELECT value FROM meta WHERE key='root'", [], |row| {
            row.get(0)
        })
        .ok()?;
    crate::store::decode_path(&encoded).ok()
}

/// Waits briefly for a stopping daemon to release its lease; `None` defers eviction.
fn acquire_daemon_lock(cache: &Path) -> Option<fs::File> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join("daemon.lock"))
        .ok()?;
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        if lock.try_lock_exclusive().is_ok() {
            return Some(lock);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests;
