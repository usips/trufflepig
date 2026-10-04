//! Review drill hints use the supported history gate and available local objects.
use crate::{history::git::run_bounded_strict, identity::GitOid};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub(super) struct HistoryDrillGate {
    deadline: Instant,
    version: Option<bool>,
    repositories: HashMap<PathBuf, bool>,
    commits: HashMap<(PathBuf, GitOid), bool>,
}

impl Default for HistoryDrillGate {
    fn default() -> Self {
        Self {
            deadline: Instant::now() + Duration::from_millis(500),
            version: None,
            repositories: HashMap::new(),
            commits: HashMap::new(),
        }
    }
}

impl HistoryDrillGate {
    pub(super) fn available(&mut self, root: &Path, common: &Path, oid: GitOid) -> bool {
        if !root.is_dir() || root.to_str().is_none() {
            return false;
        }
        let version = self.version.unwrap_or_else(|| {
            let supported = self
                .run(root, &["version"])
                .is_some_and(|bytes| version_supported(&bytes));
            self.version = Some(supported);
            supported
        });
        if !version {
            return false;
        }
        let repository = self.repositories.get(root).copied().unwrap_or_else(|| {
            let supported = !common.join("info/grafts").exists()
                && self.run(root, &["rev-parse", "--show-toplevel"]).is_some()
                && self
                    .run(
                        root,
                        &["for-each-ref", "--format=%(refname)", "refs/replace/"],
                    )
                    .is_some_and(|bytes| bytes.is_empty());
            self.repositories.insert(root.to_owned(), supported);
            supported
        });
        if !repository {
            return false;
        }
        let key = (root.to_owned(), oid);
        if let Some(available) = self.commits.get(&key) {
            return *available;
        }
        let available = self.commit_objects_available(root, oid);
        self.commits.insert(key, available);
        available
    }

    fn commit_objects_available(&self, root: &Path, oid: GitOid) -> bool {
        let Some(raw) = self.run(root, &["cat-file", "commit", oid.as_str()]) else {
            return false;
        };
        let first_parent = raw
            .split(|byte| *byte == b'\n')
            .take_while(|line| !line.is_empty())
            .find_map(|line| line.strip_prefix(b"parent "));
        if !self.tree_objects_available(root, oid) {
            return false;
        }
        first_parent.is_none_or(|parent| {
            std::str::from_utf8(parent)
                .ok()
                .and_then(|parent| GitOid::parse(parent).ok())
                .is_some_and(|parent| self.tree_objects_available(root, parent))
        })
    }

    fn tree_objects_available(&self, root: &Path, oid: GitOid) -> bool {
        self.run(
            root,
            &[
                "rev-list",
                "--objects",
                "--missing=print",
                "--max-count=1",
                oid.as_str(),
            ],
        )
        .is_some_and(|bytes| {
            !bytes.is_empty()
                && !bytes
                    .split(|byte| *byte == b'\n')
                    .any(|line| line.starts_with(b"?"))
        })
    }

    fn run(&self, root: &Path, args: &[&str]) -> Option<Vec<u8>> {
        let timeout = self
            .deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(100));
        if timeout.is_zero() {
            return None;
        }
        run_bounded_strict(root, args, timeout).ok()
    }
}

fn version_supported(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let mut parts = text.split_whitespace().nth(2).unwrap_or("").split('.');
    match (
        parts.next().and_then(|part| part.parse::<u32>().ok()),
        parts.next().and_then(|part| part.parse::<u32>().ok()),
    ) {
        (Some(major), Some(minor)) => (major, minor) >= (2, 55),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::repo_identity::tests::GitFixture;

    #[test]
    fn history_drills_reject_shallow_boundary_with_missing_raw_parent() {
        let fixture = GitFixture::new();
        fixture.commit("root");
        let tip = fixture.commit("non-root tip");
        let clone = fixture.directory.path().join("shallow");
        let origin = format!("file://{}", fixture.root.display());
        fixture.git(&[
            "clone",
            "--quiet",
            "--depth=1",
            &origin,
            clone.to_str().unwrap(),
        ]);
        let raw = fixture.git_in(&clone, &["cat-file", "commit", tip.as_str()]);
        assert!(raw.lines().any(|line| line.starts_with("parent ")));
        assert!(clone.join(".git/shallow").is_file());
        assert!(!HistoryDrillGate::default().available(&clone, &clone.join(".git"), tip));
    }

    #[test]
    fn history_drills_refuse_old_git_and_unavailable_blob_objects() {
        if crate::board::board_test_support::git_version() < Some((2, 55)) {
            eprintln!(concat!(
                "skipping history_drills_refuse_old_git_and_unavailable_blob_objects: ",
                "requires Git >= 2.55 for history drill hints"
            ));
            return;
        }
        assert!(!version_supported(b"git version 2.43.0\n"));
        assert!(version_supported(b"git version 2.55.0\n"));
        let fixture = GitFixture::new();
        fixture.commit("root");
        std::fs::write(fixture.root.join("file.txt"), "contents\n").unwrap();
        fixture.git(&["add", "file.txt"]);
        let blob = fixture.git(&["rev-parse", ":file.txt"]);
        let oid = fixture.commit("file commit");
        let common = fixture.root.join(".git");
        assert!(HistoryDrillGate::default().available(&fixture.root, &common, oid));
        let blob = blob.trim();
        std::fs::remove_file(common.join("objects").join(&blob[..2]).join(&blob[2..])).unwrap();
        assert!(!HistoryDrillGate::default().available(&fixture.root, &common, oid));
    }
}
