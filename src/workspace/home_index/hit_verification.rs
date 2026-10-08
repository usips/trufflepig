//! Per-hit worktree checks for a parent-index answer. A hit's file differs when
//! its worktree bytes no longer hash to the revision the index recorded, which
//! catches what Git divergence misses (a parent index behind its `HEAD`, edits
//! inside the divergence reuse window). Hits read from current bytes never differ.
use super::ParentFallback;
use crate::{
    identity::ContentRevision,
    results::Hit,
    source::{self, MAX_READ_BYTES},
    store::decode_path,
};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

/// Most distinct files hashed for one answer; later files fall back to the
/// Git divergence (and count as differing when it is incomplete).
const MAX_VERIFIED_FILES: usize = 128;

/// One query's worktree hash checks: each file is hashed at most once, and at
/// most [`MAX_VERIFIED_FILES`] files are hashed in all.
#[derive(Debug, Default)]
pub(crate) struct WorktreeHashes {
    matched: HashMap<String, bool>,
}

impl WorktreeHashes {
    /// Whether `path` still has `revision`; `None` once the budget is spent
    /// on other files, leaving it unverified.
    pub(super) fn check(
        &mut self,
        root: &Path,
        path: &str,
        revision: Option<&str>,
    ) -> Option<bool> {
        if let Some(&matches) = self.matched.get(path) {
            return Some(matches);
        }
        if self.matched.len() >= MAX_VERIFIED_FILES {
            return None;
        }
        let matches = worktree_matches(root, path, revision);
        self.matched.insert(path.to_owned(), matches);
        Some(matches)
    }
}

/// Whether the worktree file at `path` still has the indexed `revision`.
/// Unreadable files and hits without a revision do not match.
fn worktree_matches(root: &Path, path: &str, revision: Option<&str>) -> bool {
    let Some(expected) = revision.and_then(|revision| ContentRevision::parse(revision).ok()) else {
        return false;
    };
    decode_path(path)
        .ok()
        .and_then(|relative| source::read_contained(root, &relative, MAX_READ_BYTES).ok())
        .is_some_and(|bytes| ContentRevision::of(&bytes) == expected)
}

/// Hits read from current worktree bytes (live regex, re-extraction).
fn reads_current_bytes(hit: &Hit) -> bool {
    matches!(
        hit.provenance.as_deref(),
        Some("live_regex" | "reextracted")
    )
}

/// Files of one parent-index answer whose worktree bytes differ from the index.
#[derive(Debug, Default)]
pub(crate) struct DifferingFiles {
    paths: HashSet<String>,
}

impl DifferingFiles {
    /// Whether `hit` shows index bytes its worktree file no longer has.
    pub(crate) fn flags(&self, hit: &Hit) -> bool {
        !reads_current_bytes(hit) && self.paths.contains(&hit.path)
    }
}

impl ParentFallback {
    /// Files among `hits` whose worktree bytes differ, plus `changed` files an
    /// answer already re-read. Files beyond the hash budget fall back to the
    /// Git divergence and count as differing when it is incomplete.
    pub(crate) fn check_hits(
        &self,
        root: &Path,
        hits: &[Hit],
        hashes: &mut WorktreeHashes,
        changed: HashSet<String>,
    ) -> DifferingFiles {
        let mut paths = changed;
        for hit in hits.iter().filter(|hit| !reads_current_bytes(hit)) {
            if paths.contains(&hit.path) {
                continue;
            }
            let differs = match hashes.check(root, &hit.path, hit.revision.as_deref()) {
                Some(matches) => !matches,
                None => self.divergence.contains(&hit.path) || !self.divergence.complete,
            };
            if differs {
                paths.insert(hit.path.clone());
            }
        }
        DifferingFiles { paths }
    }

    /// Files known to differ: the Git divergence plus files whose hits failed
    /// the hash check; `None` when the divergence is incomplete.
    pub(crate) fn known_differing(&self, differing: &DifferingFiles) -> Option<usize> {
        self.divergence.complete.then(|| {
            let extra = differing
                .paths
                .iter()
                .filter(|path| !self.divergence.contains(path))
                .count();
            self.divergence.paths.len() + extra
        })
    }
}
