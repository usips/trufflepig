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

/// Whether the worktree file at `path` still has the indexed `revision`.
/// Unreadable files and hits without a revision do not match.
pub(super) fn worktree_matches(root: &Path, path: &str, revision: Option<&str>) -> bool {
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
pub(in crate::workspace) struct DifferingFiles {
    paths: HashSet<String>,
}

impl DifferingFiles {
    /// Whether `hit` shows index bytes its worktree file no longer has.
    pub(in crate::workspace) fn flags(&self, hit: &Hit) -> bool {
        !reads_current_bytes(hit) && self.paths.contains(&hit.path)
    }
}

impl ParentFallback {
    /// Checks each distinct file among `hits` (first [`MAX_VERIFIED_FILES`] by
    /// hash, then by divergence), hashing each file once.
    pub(in crate::workspace) fn check_hits(&self, root: &Path, hits: &[Hit]) -> DifferingFiles {
        let mut checked: HashMap<&str, bool> = HashMap::with_capacity(hits.len().min(256));
        for hit in hits.iter().filter(|hit| !reads_current_bytes(hit)) {
            if checked.contains_key(hit.path.as_str()) {
                continue;
            }
            let differs = if checked.len() < MAX_VERIFIED_FILES {
                !worktree_matches(root, &hit.path, hit.revision.as_deref())
            } else {
                self.divergence.contains(&hit.path) || !self.divergence.complete
            };
            checked.insert(&hit.path, differs);
        }
        DifferingFiles {
            paths: checked
                .into_iter()
                .filter(|(_, differs)| *differs)
                .map(|(path, _)| path.to_owned())
                .collect(),
        }
    }

    /// Files known to differ: the Git divergence plus files whose hits failed
    /// the hash check; `None` when the divergence is incomplete.
    pub(super) fn known_differing(&self, differing: &DifferingFiles) -> Option<usize> {
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
