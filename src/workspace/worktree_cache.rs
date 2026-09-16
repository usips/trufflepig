//! Index cache for a member root that is a linked worktree: its own directory
//! keyed by the worktree path, warmed from the configured member's cache when
//! that cache exists. Never shares one database between two roots.

use anyhow::Result;
use std::path::{Path, PathBuf};

/// Directory for `effective`'s index, seeded from `configured`'s cache.
pub(crate) fn ensure(
    configured: &Path,
    effective: &Path,
    explicit: Option<&Path>,
) -> Result<PathBuf> {
    let member_cache = super::hashed_member_cache(configured, explicit)?;
    let worktree_cache = super::hashed_member_cache(effective, explicit)?;
    std::fs::create_dir_all(&worktree_cache)?;
    seed(&member_cache, effective, &worktree_cache);
    Ok(worktree_cache)
}

/// Warm start; a missing or oversize source simply leaves a fresh cache.
fn seed(member_cache: &Path, effective: &Path, worktree_cache: &Path) {
    if worktree_cache.join("index.sqlite3").exists() {
        return;
    }
    let _ = crate::store::ensure_seeded(member_cache, effective, worktree_cache);
}
