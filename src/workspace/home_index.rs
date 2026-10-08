//! Which index answers for a member: its own published index or, for a linked
//! worktree whose own index is still warming, the parent member's published
//! index reading the worktree's current bytes ([`Store::reading_from`]). Paths
//! are root-relative, so they address the same files; hits in files that may
//! differ are flagged, and changed definitions are re-extracted from worktree bytes.
mod hit_verification;
mod parent_show;
#[cfg(test)]
mod tests;
mod worktree_divergence;
mod worktree_reads;

use super::member_root::{MemberRoot, linked_root_of_member};
use crate::{
    daemon::deadline::{QueryDeadline, TIMED_OUT},
    store::{Store, is_index_cannot_open, is_index_warming},
};
use anyhow::{Context, Result, ensure};
pub(super) use hit_verification::DifferingFiles;
pub(crate) use hit_verification::WorktreeHashes;
pub(super) use parent_show::acquire_home_read;
pub(crate) use parent_show::{acquire_through_parent, reextracted_entry};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{os::unix::fs::MetadataExt, path::Path, time::Duration};
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
pub(crate) struct ParentIndexView {
    pub store: Store,
    pub fallback: ParentFallback,
}

/// What a parent-index answer must disclose and repair for its worktree.
pub(crate) struct ParentFallback {
    pub divergence: WorktreeDivergence,
    /// `MEMBER index`, the index that answered.
    pub served_from: String,
}

/// Immutable index identity attached to result sets answered by a parent.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ParentIndexIdentity {
    pub root: String,
    pub index_epoch: String,
    pub generation: i64,
    pub served_from: String,
    pub worktree_root: String,
    pub worktree_device: u64,
    pub worktree_inode: u64,
}

impl ParentIndexIdentity {
    pub(crate) fn validate_publication(&self, store: &Store) -> Result<()> {
        let publication = store
            .publication()?
            .context("stale_result: saved parent index is unpublished; search again")?;
        ensure!(
            publication.root == self.root
                && publication.index_epoch == self.index_epoch
                && publication.generation == self.generation,
            "stale_result: parent index changed; search again"
        );
        Ok(())
    }

    pub(crate) fn capture(store: &Store, fallback: &ParentFallback) -> Result<Self> {
        let publication = store
            .publication()?
            .context("stale_result: parent index has no published identity")?;
        let worktree = store.root.metadata()?;
        Ok(Self {
            root: crate::store::encode_path(store.index_root()),
            index_epoch: publication.index_epoch,
            generation: publication.generation,
            served_from: fallback.served_from.clone(),
            worktree_root: crate::store::encode_path(&store.root),
            worktree_device: worktree.dev(),
            worktree_inode: worktree.ino(),
        })
    }
}

/// Uses a published main-checkout index for a standalone linked worktree only
/// when its caller already selected the worktree's default cache. A custom
/// cache remains isolated and is never guessed from Git metadata.
pub(crate) fn standalone_parent_view(
    root: &Path,
    cache: &Path,
    deadline: QueryDeadline,
) -> Option<ParentIndexView> {
    let root = root.canonicalize().ok()?;
    let default_cache = crate::cli::cache_path(&root, None).ok()?;
    if cache != default_cache {
        return None;
    }
    let member = super::member_root::standalone_worktree(&root)?;
    std::fs::create_dir_all(cache).ok()?;
    crate::system::sweep::record_cache_root(cache, &root);
    match resolve_home_index(
        &member,
        cache,
        None,
        HomeIndexPolicy::AwaitDaemon(Duration::ZERO),
        deadline,
    ) {
        HomeIndexSource::Parent(view) => Some(view),
        _ => None,
    }
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
        // A deadline interrupt stays recognizable however much context wraps it.
        Err(error) => HomeIndexSource::Unavailable {
            reason: match deadline.classify(error) {
                error
                    if error.chain().any(|cause| {
                        cause
                            .to_string()
                            .strip_prefix(TIMED_OUT)
                            .is_some_and(|rest| rest.starts_with(':'))
                    }) =>
                {
                    format!("{TIMED_OUT}: query deadline expired")
                }
                error => format!("{error:#}"),
            },
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
    let mut cannot_open = None;
    loop {
        match Store::open_read(&member.root, member_cache, deadline) {
            Err(error) if is_index_warming(&error) => {}
            Err(error) if is_index_cannot_open(&error) && member.worktree.is_some() => {
                cannot_open = Some(error);
            }
            opened => return Ok(HomeIndexSource::Own(opened?)),
        }
        if member.worktree.is_some()
            && let Some(view) = parent_view(member, member_cache, explicit_cache, deadline)
        {
            return Ok(HomeIndexSource::Parent(view));
        }
        if std::time::Instant::now() >= until {
            if let Some(error) = cannot_open {
                return Ok(HomeIndexSource::Unavailable {
                    reason: format!(
                        "index_cache_unavailable: could not open the linked worktree index; check cache permissions or run `trufflepig index`: {error:#}"
                    ),
                });
            }
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
    /// Records the fallback on a member's coverage row: `state: parent_fallback`,
    /// the home's own state, the answering index, and what differs.
    pub(crate) fn describe(&self, row: &mut Value, differing: &DifferingFiles, hits: usize) {
        row["state"] = "parent_fallback".into();
        row["home_state"] = "warming".into();
        row["served_from"] = self.served_from.clone().into();
        row["differs"] = self.known_differing(differing).into();
        row["differing_hits"] = hits.into();
    }
}
