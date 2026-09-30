//! Files whose linked-worktree bytes may differ from the parent member's
//! published index: worktree changes against the parent's `HEAD` (committed,
//! staged, unstaged, untracked) plus the parent's own uncommitted and untracked
//! files. Each Git probe is bounded; one that fails or times out leaves the set
//! incomplete. A result is reused from `<worktree cache>/divergence.json` briefly.

use crate::{history::git, store::encode_path};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Longest one Git probe may run before the divergence is reported incomplete.
const GIT_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a stored divergence answers for an unchanged [`DivergenceKey`].
const DIVERGENCE_REUSE_MS: u64 = 5_000;
const DIVERGENCE_FILE: &str = "divergence.json";

/// Root-relative, percent-encoded paths (as indexed) that may differ between a
/// worktree and its parent's index.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorktreeDivergence {
    pub paths: BTreeSet<String>,
    /// False when a probe failed, so files outside `paths` may differ too.
    pub complete: bool,
}

/// What a stored divergence was computed from; any change recomputes it.
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
struct DivergenceKey {
    parent_head: String,
    worktree_head: String,
    parent_git_index: GitIndexStamp,
    worktree_git_index: GitIndexStamp,
    parent_generation: i64,
}

/// Modification time and size of a Git index file (`None` when absent).
type GitIndexStamp = Option<(i64, i64, u64)>;

#[derive(Deserialize, Serialize)]
struct StoredDivergence {
    key: DivergenceKey,
    computed_ms: u64,
    divergence: WorktreeDivergence,
}

impl WorktreeDivergence {
    /// The divergence between `worktree_root` and the index of `parent_root` at
    /// `parent_generation`, reused from `worktree_cache` when fresh.
    pub fn load_or_compute(
        parent_root: &Path,
        worktree_root: &Path,
        worktree_cache: &Path,
        parent_generation: i64,
    ) -> Self {
        let (parent, worktree) = std::thread::scope(|scope| {
            let parent = scope.spawn(|| head_and_index(parent_root));
            (parent.join().ok().flatten(), head_and_index(worktree_root))
        });
        let key = match (parent, worktree) {
            (Some((parent_head, parent_git_index)), Some((worktree_head, worktree_git_index))) => {
                DivergenceKey {
                    parent_head,
                    worktree_head,
                    parent_git_index,
                    worktree_git_index,
                    parent_generation,
                }
            }
            (parent, _) => {
                let parent_head = parent.map(|(head, _)| head);
                let mut divergence = compute(parent_root, worktree_root, parent_head.as_deref());
                divergence.complete = false;
                return divergence;
            }
        };
        let stored_path = worktree_cache.join(DIVERGENCE_FILE);
        let now = now_ms();
        if let Some(stored) = std::fs::read(&stored_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<StoredDivergence>(&bytes).ok())
            && stored.key == key
            && now.saturating_sub(stored.computed_ms) <= DIVERGENCE_REUSE_MS
        {
            return stored.divergence;
        }
        let divergence = compute(parent_root, worktree_root, Some(&key.parent_head));
        store(
            worktree_cache,
            &StoredDivergence {
                key,
                computed_ms: now,
                divergence: divergence.clone(),
            },
        );
        divergence
    }

    pub fn contains(&self, path: &str) -> bool {
        self.paths.contains(path)
    }

    /// Number of differing files, or `None` when the set is incomplete.
    pub fn known_count(&self) -> Option<usize> {
        self.complete.then_some(self.paths.len())
    }
}

/// Runs the four probes concurrently; `parent_head` is required for the first.
fn compute(
    parent_root: &Path,
    worktree_root: &Path,
    parent_head: Option<&str>,
) -> WorktreeDivergence {
    let listed = std::thread::scope(|scope| {
        let probes = [
            parent_head.map(|head| {
                let args = ["diff", "--name-only", "-z", "--no-renames", head, "--"];
                scope.spawn(move || git::run_bounded(worktree_root, &args, GIT_PROBE_TIMEOUT))
            }),
            Some(scope.spawn(|| {
                let args = ["ls-files", "-z", "-o", "--exclude-standard"];
                git::run_bounded(worktree_root, &args, GIT_PROBE_TIMEOUT)
            })),
            Some(scope.spawn(|| {
                let args = ["diff", "--name-only", "-z", "--no-renames", "HEAD", "--"];
                git::run_bounded(parent_root, &args, GIT_PROBE_TIMEOUT)
            })),
            Some(scope.spawn(|| {
                let args = ["ls-files", "-z", "-o", "--exclude-standard"];
                git::run_bounded(parent_root, &args, GIT_PROBE_TIMEOUT)
            })),
        ];
        probes.map(|probe| probe.and_then(|handle| handle.join().ok()?.ok()))
    });
    let mut divergence = WorktreeDivergence {
        paths: BTreeSet::new(),
        complete: listed.iter().all(Option::is_some),
    };
    for output in listed.iter().flatten() {
        divergence.paths.extend(
            output
                .split(|&byte| byte == 0)
                .filter(|path| !path.is_empty())
                .map(|path| encode_path(Path::new(OsStr::from_bytes(path)))),
        );
    }
    divergence
}

/// `HEAD` and the Git index stamp of the checkout at `root`.
fn head_and_index(root: &Path) -> Option<(String, GitIndexStamp)> {
    let output = git::run_bounded(
        root,
        &["rev-parse", "HEAD", "--git-path", "index"],
        GIT_PROBE_TIMEOUT,
    )
    .ok()?;
    let mut lines = output.split(|&byte| byte == b'\n');
    let head = std::str::from_utf8(lines.next()?).ok()?.to_owned();
    let index = PathBuf::from(OsStr::from_bytes(lines.next()?));
    let stamp = std::fs::metadata(root.join(index))
        .ok()
        .map(|metadata| (metadata.mtime(), metadata.mtime_nsec(), metadata.size()));
    Some((head, stamp))
}

/// Best effort: a failed write only means the next query recomputes.
fn store(worktree_cache: &Path, stored: &StoredDivergence) {
    let Ok(bytes) = serde_json::to_vec(stored) else {
        return;
    };
    let staged = worktree_cache.join(format!(
        "{DIVERGENCE_FILE}.{}",
        uuid::Uuid::new_v4().simple()
    ));
    if std::fs::write(&staged, bytes).is_err()
        || std::fs::rename(&staged, worktree_cache.join(DIVERGENCE_FILE)).is_err()
    {
        let _ = std::fs::remove_file(staged);
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}
