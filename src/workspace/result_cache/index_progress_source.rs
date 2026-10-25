use crate::{
    path_codec::{decode_path, encode_path},
    store::{IndexProgress, read_index_progress},
};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Canonical selected paths used to refresh transient index progress on a page.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct IndexProgressSource {
    root: String,
    cache: String,
}

impl IndexProgressSource {
    /// Captures paths only; callers verify the selected member before capture.
    pub(crate) fn capture(root: &Path, cache: &Path) -> Option<Self> {
        let root = root.canonicalize().ok()?;
        let cache = cache.canonicalize().ok()?;
        Some(Self {
            root: encode_path(&root),
            cache: encode_path(&cache),
        })
    }

    /// Reads current progress only while both saved paths remain canonical.
    pub(crate) fn read(&self) -> Option<IndexProgress> {
        let root = decode_path(&self.root).ok()?;
        let cache = decode_path(&self.cache).ok()?;
        if root.canonicalize().ok()? != root || cache.canonicalize().ok()? != cache {
            return None;
        }
        read_index_progress(&root, &cache)
    }
}
