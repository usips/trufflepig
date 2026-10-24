//! Process-local memo of pinned model assets whose SHA-256 already matched.
//! A model lane owns one for its lifetime so reloads skip re-hashing
//! unchanged files; any change to a file's identity forces a fresh hash.

use std::{fs::Metadata, os::unix::fs::MetadataExt};

/// A file's identity for digest reuse: device, inode, size, and mtime in
/// nanoseconds. Replacing or rewriting the file changes at least one field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AssetFileIdentity {
    device: u64,
    inode: u64,
    size: u64,
    modified_ns: i128,
}

impl AssetFileIdentity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.size(),
            modified_ns: i128::from(metadata.mtime()) * 1_000_000_000
                + i128::from(metadata.mtime_nsec()),
        }
    }
}

/// Pinned-asset digests verified by one owner, keyed by [`AssetFileIdentity`].
/// Holds a handful of entries (one per pinned file), so lookup is linear.
#[derive(Debug, Default)]
pub struct VerifiedAssetDigests {
    verified: Vec<(AssetFileIdentity, String)>,
}

#[cfg_attr(not(feature = "semantic"), allow(dead_code))]
impl VerifiedAssetDigests {
    /// True when a file with this identity already hashed to `sha256`.
    pub(crate) fn contains(&self, metadata: &Metadata, sha256: &str) -> bool {
        let identity = AssetFileIdentity::of(metadata);
        self.verified
            .iter()
            .any(|(known, digest)| *known == identity && digest == sha256)
    }

    /// Records that the file with this identity hashed to `sha256`.
    pub(crate) fn record(&mut self, metadata: &Metadata, sha256: &str) {
        let identity = AssetFileIdentity::of(metadata);
        self.verified.retain(|(known, _)| *known != identity);
        self.verified.push((identity, sha256.to_owned()));
    }
}
