//! The bounded current-source walk: path order inside the selected root, no
//! symlinks, `.git`/`target`/`node_modules`/`.trufflepig` and caches excluded.
use super::{
    coverage::{CurrentCoverage, LIMIT_DEADLINE, LIMIT_WALK},
    read_cutoff::ReadCutoff,
};
use crate::{search::InvocationDirectory, store::Store};
use anyhow::Result;
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

pub(super) const MAX_WALK_ENTRIES: usize = 65_536;

/// Root-relative encoded paths of the regular files under `start` (the root
/// or a directory inside it), at most `max_paths`, stopping at the cutoff.
pub(super) fn current_paths(
    store: &Store,
    start: &Path,
    cache_exclusion: &Path,
    max_paths: usize,
    cutoff: &mut ReadCutoff,
    current: &mut CurrentCoverage,
) -> Result<Vec<String>> {
    let mut caches = vec![
        normalized_path(cache_exclusion),
        normalized_path(store.results_cache()),
    ];
    caches.sort();
    caches.dedup();
    let mut walker = WalkBuilder::new(start);
    walker
        .hidden(false)
        .require_git(false)
        .follow_links(false)
        .sort_by_file_path(|left, right| left.cmp(right));
    walker.filter_entry(move |entry| {
        entry.file_name() != ".git"
            && !caches.iter().any(|cache| entry.path().starts_with(cache))
            && (!entry.file_type().is_some_and(|kind| kind.is_dir())
                || !matches!(
                    entry.file_name().to_str(),
                    Some("target" | "node_modules" | ".trufflepig")
                ))
    });
    let mut paths = Vec::with_capacity(max_paths.min(MAX_WALK_ENTRIES));
    let mut entries = 0usize;
    for entry in walker.build() {
        if cutoff.reached() {
            current.path_catalog_truncated = true;
            current.unsearched_unknown = true;
            current.stop(LIMIT_DEADLINE, 0, None);
            break;
        }
        entries += 1;
        if entries > MAX_WALK_ENTRIES {
            truncate_catalog(current, None);
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                current.walk_failures += 1;
                current.complete = false;
                continue;
            }
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        current.walked_files += 1;
        let relative = tp_core::path_codec::encode_path(entry.path().strip_prefix(&store.root)?);
        if paths.len() == max_paths {
            truncate_catalog(current, Some(&relative));
            break;
        }
        paths.push(relative);
    }
    Ok(paths)
}

fn truncate_catalog(current: &mut CurrentCoverage, first: Option<&str>) {
    current.path_catalog_truncated = true;
    current.unsearched_lower_bound = true;
    current.stop(LIMIT_WALK, 1, first);
}

/// Moves files under the invocation directory to the front, keeping path order
/// within each part, so a deadline-limited read covers the caller's area first.
pub(super) fn order_for_reading(paths: &mut [String], origin: &InvocationDirectory) {
    paths.sort_by_key(|path| !origin.encloses(path));
}

pub(super) fn normalized_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|current| current.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        }
    })
}
