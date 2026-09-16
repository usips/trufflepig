//! Evicts default-base root caches whose recorded root no longer exists.
//! Only directories holding `index.sqlite3` are candidates, a grace period
//! protects caches being created or written, and a root whose parent is also
//! gone is left alone because it may be an unmounted volume.

use fs2::FileExt;
use rusqlite::Connection;
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

/// One removed cache directory and the root it served.
#[derive(Clone, Debug, Serialize)]
pub struct Evicted {
    pub cache: PathBuf,
    pub root: PathBuf,
    pub daemon_stopped: bool,
}

/// Pure over `base`; removes caches whose recorded root is gone. Never errors on one entry.
pub fn sweep(base: &Path, now: SystemTime, grace: Duration) -> Vec<Evicted> {
    let Ok(entries) = fs::read_dir(base) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| sweep_entry(&entry.path(), now, grace))
        .collect()
}

fn sweep_entry(cache: &Path, now: SystemTime, grace: Duration) -> Option<Evicted> {
    let directory = fs::metadata(cache).ok().filter(Metadata::is_dir)?;
    let index_path = cache.join("index.sqlite3");
    let index = fs::metadata(&index_path).ok().filter(Metadata::is_file)?;
    if modified_within(&directory, now, grace) || modified_within(&index, now, grace) {
        return None;
    }
    let root = recorded_root(&index_path)?;
    if root.is_dir() || !root.parent().is_some_and(Path::is_dir) {
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

/// A missing or future modification time counts as recent.
fn modified_within(metadata: &Metadata, now: SystemTime, grace: Duration) -> bool {
    metadata.modified().ok().is_none_or(|modified| {
        now.duration_since(modified).is_ok_and(|age| age < grace) || now < modified
    })
}

fn recorded_root(index: &Path) -> Option<PathBuf> {
    let conn = Connection::open(index).ok()?;
    conn.busy_timeout(Duration::from_secs(1)).ok()?;
    conn.execute_batch("PRAGMA query_only=ON").ok()?;
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
mod tests {
    use super::*;
    use crate::store::Store;

    fn scratch() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("trufflepig-sweep-")
            .tempdir_in(
                std::env::var_os("TMPDIR").expect("TMPDIR must name disk-backed scratch space"),
            )
            .unwrap()
    }

    /// Creates a published cache under `base` for `root`, returning both paths.
    fn cache_for(base: &Path, root: &Path, name: &str) -> PathBuf {
        fs::create_dir_all(root).unwrap();
        fs::write(root.join("lib.rs"), "fn swept() {}\n").unwrap();
        let cache = base.join(name);
        Store::open(root, &cache).unwrap().index().unwrap();
        cache
    }

    fn age(path: &Path, by: Duration) {
        fs::File::open(path)
            .unwrap()
            .set_modified(SystemTime::now() - by)
            .unwrap();
    }

    fn age_cache(cache: &Path) {
        age(&cache.join("index.sqlite3"), 2 * SWEEP_GRACE);
        age(cache, 2 * SWEEP_GRACE);
    }

    #[test]
    fn sweep_evicts_vanished_root_and_keeps_live_root() {
        let scratch = scratch();
        let base = scratch.path().join("base");
        let roots = scratch.path().join("roots");
        let live = cache_for(&base, &roots.join("live"), "live-cache");
        let gone = cache_for(&base, &roots.join("gone"), "gone-cache");
        let gone_root = roots.join("gone").canonicalize().unwrap();
        fs::remove_dir_all(&gone_root).unwrap();
        age_cache(&live);
        age_cache(&gone);
        let evicted = sweep(&base, SystemTime::now(), SWEEP_GRACE);
        assert_eq!(evicted.len(), 1, "{evicted:?}");
        assert_eq!(evicted[0].cache, gone);
        assert_eq!(evicted[0].root, gone_root);
        assert!(!evicted[0].daemon_stopped);
        assert!(!gone.exists());
        assert!(live.join("index.sqlite3").is_file());
        assert!(sweep(&base, SystemTime::now(), SWEEP_GRACE).is_empty());
    }

    #[test]
    fn sweep_respects_grace_period() {
        let scratch = scratch();
        let base = scratch.path().join("base");
        let root = scratch.path().join("roots").join("gone");
        let cache = cache_for(&base, &root, "gone-cache");
        fs::remove_dir_all(&root).unwrap();
        assert!(sweep(&base, SystemTime::now(), SWEEP_GRACE).is_empty());
        assert!(cache.join("index.sqlite3").is_file());
        age(&cache.join("index.sqlite3"), 2 * SWEEP_GRACE);
        assert!(sweep(&base, SystemTime::now(), SWEEP_GRACE).is_empty());
        age(&cache, 2 * SWEEP_GRACE);
        assert_eq!(sweep(&base, SystemTime::now(), SWEEP_GRACE).len(), 1);
        assert!(!cache.exists());
    }

    #[test]
    fn sweep_ignores_dirs_without_index() {
        let scratch = scratch();
        let base = scratch.path().join("base");
        let history = base.join("history");
        fs::create_dir_all(&history).unwrap();
        fs::write(history.join("history.sqlite3"), b"kept").unwrap();
        fs::write(base.join("stray-file"), b"kept").unwrap();
        age(&history, 2 * SWEEP_GRACE);
        assert!(sweep(&base, SystemTime::now(), SWEEP_GRACE).is_empty());
        assert!(history.join("history.sqlite3").is_file());
        assert!(
            sweep(
                &scratch.path().join("missing"),
                SystemTime::now(),
                SWEEP_GRACE
            )
            .is_empty()
        );
    }

    #[test]
    fn sweep_skips_root_whose_parent_is_missing() {
        let scratch = scratch();
        let base = scratch.path().join("base");
        let volume = scratch.path().join("volume");
        let cache = cache_for(&base, &volume.join("repo"), "volume-cache");
        fs::remove_dir_all(&volume).unwrap();
        age_cache(&cache);
        assert!(sweep(&base, SystemTime::now(), SWEEP_GRACE).is_empty());
        assert!(cache.join("index.sqlite3").is_file());
        fs::create_dir(&volume).unwrap();
        // Reading the index touches the directory; age it again for the second pass.
        age_cache(&cache);
        assert_eq!(sweep(&base, SystemTime::now(), SWEEP_GRACE).len(), 1);
        assert!(!cache.exists());
    }
}
