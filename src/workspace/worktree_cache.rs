//! Index cache for a member root that is a linked worktree: its own directory
//! keyed by the worktree path, warmed from the configured member's cache when
//! that cache exists. Never shares one database between two roots.

use crate::daemon::deadline::{QueryDeadline, TIMED_OUT};
use anyhow::{Result, ensure};
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

/// Directory for `effective`'s index, seeded from `configured`'s cache.
pub(crate) fn ensure(
    configured: &Path,
    effective: &Path,
    explicit: Option<&Path>,
) -> Result<PathBuf> {
    let member_cache = super::hashed_member_cache(configured, explicit)?;
    let worktree_cache = super::hashed_member_cache(effective, explicit)?;
    std::fs::create_dir_all(&worktree_cache)?;
    // Parent-index reads leave results here before any index exists.
    crate::system::sweep::record_cache_root(&worktree_cache, effective);
    seed(&member_cache, effective, &worktree_cache);
    Ok(worktree_cache)
}

/// Directory for `effective` prepared under the caller's original deadline.
pub(super) fn ensure_until(
    configured: &Path,
    effective: &Path,
    explicit: Option<&Path>,
    deadline: &QueryDeadline,
) -> Result<PathBuf> {
    ensure!(!deadline.expired(), "{TIMED_OUT}: query deadline expired");
    let member_cache = super::hashed_member_cache(configured, explicit)?;
    let worktree_cache = super::hashed_member_cache(effective, explicit)?;
    std::fs::create_dir_all(&worktree_cache)
        .map_err(|error| crate::store::index_cache_unavailable(&worktree_cache, error.into()))?;
    ensure!(!deadline.expired(), "{TIMED_OUT}: query deadline expired");
    crate::system::sweep::record_cache_root(&worktree_cache, effective);
    ensure!(!deadline.expired(), "{TIMED_OUT}: query deadline expired");
    let _ = crate::store::ensure_seeded_until(&member_cache, effective, &worktree_cache, deadline);
    ensure!(!deadline.expired(), "{TIMED_OUT}: query deadline expired");
    Ok(worktree_cache)
}

/// Warm start; a missing or oversize source simply leaves a fresh cache.
fn seed(member_cache: &Path, effective: &Path, worktree_cache: &Path) {
    if worktree_cache.join("index.sqlite3").exists() {
        return;
    }
    let _ = crate::store::ensure_seeded(member_cache, effective, worktree_cache);
}
