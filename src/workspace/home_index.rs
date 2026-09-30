//! Which index answers for a member: its own published index or, for a linked
//! worktree whose own index is still warming, the parent member's published
//! index reading the worktree's current bytes ([`Store::reading_from`]). Paths
//! are root-relative, so they address the same files; hits in files that may
//! differ are flagged, and changed definitions are re-extracted from worktree bytes.
mod parent_show;
#[cfg(test)]
mod tests;
mod worktree_divergence;
mod worktree_reads;

use super::member_root::{MemberRoot, linked_root_of_member};
use crate::{
    daemon::deadline::QueryDeadline,
    store::{Store, is_index_warming},
};
use anyhow::Result;
pub(super) use parent_show::{acquire_home_read, reextracted_entry};
use serde_json::Value;
use std::{path::Path, time::Duration};
pub(super) use worktree_divergence::WorktreeDivergence;

/// How long a daemon-backed query waits for the home member's first publication
/// when no parent index can answer for it.
pub(super) const HOME_PUBLICATION_WAIT: Duration = Duration::from_secs(8);
/// Warming reason of a linked worktree whose parent member has no published index.
pub(super) const NO_PARENT_INDEX: &str = "no parent index";

/// How a query obtains a member's index.
#[derive(Clone, Copy, Debug)]
pub(super) enum HomeIndexPolicy {
    /// `--no-daemon`: the caller indexed the member in-process (outside the
    /// query deadline), so its own index is read without waiting.
    IndexInline,
    /// Read what daemons have published; the home member waits up to this long.
    AwaitDaemon(Duration),
}

impl HomeIndexPolicy {
    pub(super) fn for_search(no_daemon: bool) -> Self {
        if no_daemon {
            Self::IndexInline
        } else {
            Self::AwaitDaemon(HOME_PUBLICATION_WAIT)
        }
    }
}

/// The index that answers for one member.
pub(super) enum HomeIndexSource {
    Own(Store),
    Parent(ParentIndexView),
    Warming { reason: Option<&'static str> },
    Unavailable { reason: String },
}

/// A parent member's published index reading a warming worktree's bytes.
pub(super) struct ParentIndexView {
    pub store: Store,
    pub fallback: ParentFallback,
}

/// What a parent-index answer must disclose and repair for its worktree.
pub(super) struct ParentFallback {
    pub divergence: WorktreeDivergence,
    /// `MEMBER index`, the index that answered.
    pub served_from: String,
}

/// Opens the index that answers for `member` under `policy`. Only a linked
/// worktree falls back to its parent, and it does so without waiting.
pub(super) fn resolve_home_index(
    member: &MemberRoot,
    member_cache: &Path,
    explicit_cache: Option<&Path>,
    policy: HomeIndexPolicy,
    deadline: QueryDeadline,
) -> HomeIndexSource {
    match resolve(member, member_cache, explicit_cache, policy, deadline) {
        Ok(source) => source,
        Err(error) => HomeIndexSource::Unavailable {
            reason: format!("{error:#}"),
        },
    }
}

fn resolve(
    member: &MemberRoot,
    member_cache: &Path,
    explicit_cache: Option<&Path>,
    policy: HomeIndexPolicy,
    deadline: QueryDeadline,
) -> Result<HomeIndexSource> {
    let wait = match policy {
        HomeIndexPolicy::IndexInline => {
            let store = Store::open_read(&member.root, member_cache, deadline)?;
            return Ok(HomeIndexSource::Own(store));
        }
        HomeIndexPolicy::AwaitDaemon(wait) if member.is_home => deadline.cap(wait),
        HomeIndexPolicy::AwaitDaemon(_) => Duration::ZERO,
    };
    let until = std::time::Instant::now() + wait;
    loop {
        match Store::open_read(&member.root, member_cache, deadline) {
            Err(error) if is_index_warming(&error) => {}
            opened => return Ok(HomeIndexSource::Own(opened?)),
        }
        if member.worktree.is_some()
            && let Some(view) = parent_view(member, member_cache, explicit_cache, deadline)
        {
            return Ok(HomeIndexSource::Parent(view));
        }
        if std::time::Instant::now() >= until {
            return Ok(HomeIndexSource::Warming {
                reason: member.worktree.is_some().then_some(NO_PARENT_INDEX),
            });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The configured member's published index reading `member`'s worktree bytes;
/// `None` while the parent is unpublished or unreadable.
fn parent_view(
    member: &MemberRoot,
    member_cache: &Path,
    explicit_cache: Option<&Path>,
    deadline: QueryDeadline,
) -> Option<ParentIndexView> {
    let parent_root = &member.member.root;
    let parent_cache = super::hashed_member_cache(parent_root, explicit_cache).ok()?;
    let parent = Store::open_read(parent_root, &parent_cache, deadline).ok()?;
    // Only a linked worktree sharing the parent's Git common directory may read through it.
    linked_root_of_member(&member.member, &member.root)?;
    let store = parent.reading_from(&member.root, member_cache).ok()?;
    let divergence = WorktreeDivergence::load_or_compute(
        parent_root,
        &store.root,
        member_cache,
        store.generation().ok()?,
    );
    Some(ParentIndexView {
        store,
        fallback: ParentFallback {
            divergence,
            served_from: format!("{} index", member.name()),
        },
    })
}

impl ParentFallback {
    /// Whether `path` may differ between the worktree and the answering index.
    pub(super) fn differs(&self, path: &str) -> bool {
        self.divergence.contains(path)
    }

    /// Records the fallback on a member's coverage row: `state: parent_fallback`,
    /// the home's own state, the answering index, and what differs.
    pub(super) fn describe(&self, row: &mut Value, differing_hits: usize) {
        row["state"] = "parent_fallback".into();
        row["home_state"] = "warming".into();
        row["served_from"] = self.served_from.clone().into();
        row["differs"] = self.divergence.known_count().into();
        row["differing_hits"] = differing_hits.into();
    }
}
