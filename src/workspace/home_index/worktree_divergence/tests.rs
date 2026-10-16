use super::{GitIndexStamp, WorktreeDivergence, head_and_index};
use crate::store::encode_path;
use crate::workspace::test_fixture::Fixture;
use std::{
    fs::{self, File, FileTimes},
    os::unix::ffi::OsStrExt,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, UNIX_EPOCH},
};

struct TwoWorktrees {
    _fixture: Fixture,
    _worktree_parent: tempfile::TempDir,
    parent: PathBuf,
    first: PathBuf,
    second: PathBuf,
    selected_cache: PathBuf,
}

impl TwoWorktrees {
    fn new() -> Self {
        let fixture = Fixture::git_backed();
        let parent = fixture.root.path().join("engine");
        let worktree_parent = tempfile::tempdir().unwrap();
        let first = fixture.worktree("engine", &worktree_parent.path().join("first"));
        let second = fixture.worktree("engine", &worktree_parent.path().join("second"));
        fs::write(first.join("first-only.rs"), "pub struct FirstOnly;\n").unwrap();
        fs::write(second.join("second-only.rs"), "pub struct SecondOnly;\n").unwrap();
        for worktree in [&first, &second] {
            set_git_index_mtime(worktree);
        }
        let selected_cache = fixture.cache.path().join("shared-worktree-cache");
        fs::create_dir_all(&selected_cache).unwrap();
        Self {
            _fixture: fixture,
            _worktree_parent: worktree_parent,
            parent,
            first,
            second,
            selected_cache,
        }
    }

    fn assert_old_key_collision(&self) {
        assert_eq!(
            old_key(&self.parent, &self.first, 7),
            old_key(&self.parent, &self.second, 7),
            "the roots must collide under the original cache-key fields"
        );
    }
}

#[test]
fn load_or_compute_does_not_reuse_another_worktrees_divergence() {
    let fixture = TwoWorktrees::new();
    fixture.assert_old_key_collision();

    let first = WorktreeDivergence::load_or_compute(
        &fixture.parent,
        &fixture.first,
        &fixture.selected_cache,
        7,
    );
    assert!(first.complete, "{first:?}");
    assert!(first.contains("first-only.rs"), "{first:?}");

    let second = WorktreeDivergence::load_or_compute(
        &fixture.parent,
        &fixture.second,
        &fixture.selected_cache,
        7,
    );
    assert!(second.complete, "{second:?}");
    assert!(second.contains("second-only.rs"), "{second:?}");
    assert!(!second.contains("first-only.rs"), "{second:?}");
}

#[test]
fn refresh_stores_divergence_for_its_canonical_worktree_root() {
    let fixture = TwoWorktrees::new();
    fixture.assert_old_key_collision();

    let alias = fixture._worktree_parent.path().join("first-alias");
    symlink(&fixture.first, &alias).unwrap();
    let refreshed =
        WorktreeDivergence::refresh(&fixture.parent, &alias, &fixture.selected_cache, 7);
    assert!(refreshed.contains("first-only.rs"), "{refreshed:?}");
    let stored: serde_json::Value = serde_json::from_slice(
        &fs::read(fixture.selected_cache.join(super::DIVERGENCE_FILE)).unwrap(),
    )
    .unwrap();
    assert_eq!(
        stored["key"]["worktree_root"],
        encode_path(&fixture.first.canonicalize().unwrap())
    );

    let second = WorktreeDivergence::load_or_compute(
        &fixture.parent,
        &fixture.second,
        &fixture.selected_cache,
        7,
    );
    assert!(second.contains("second-only.rs"), "{second:?}");
    assert!(!second.contains("first-only.rs"), "{second:?}");
}

fn old_key(
    parent_root: &Path,
    worktree_root: &Path,
    generation: i64,
) -> (String, String, GitIndexStamp, GitIndexStamp, i64) {
    let (parent_head, parent_index) = head_and_index(parent_root).unwrap();
    let (worktree_head, worktree_index) = head_and_index(worktree_root).unwrap();
    (
        parent_head,
        worktree_head,
        parent_index,
        worktree_index,
        generation,
    )
}

fn set_git_index_mtime(root: &Path) {
    let output = Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--git-path", "index"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let path = PathBuf::from(std::ffi::OsStr::from_bytes(
        output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout),
    ));
    let index = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    File::open(index)
        .unwrap()
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000)))
        .unwrap();
}
