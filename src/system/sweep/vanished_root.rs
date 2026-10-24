//! Decides whether a missing root was deleted or may sit on an unmounted volume.
//! The nearest existing ancestor of a deleted root is an ordinary directory on the
//! cache base's filesystem. Any other ancestor (a mount point, another device, or
//! a non-directory entry) may hide an unmounted volume, so its caches are kept.

use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

/// True when `root` is gone and its nearest existing ancestor is a plain directory
/// on `base`'s device that is not itself a mount point.
pub(super) fn root_deleted(root: &Path, base: &Path) -> bool {
    if fs::symlink_metadata(root).is_ok() {
        return false;
    }
    let Some(ancestor) = nearest_existing_ancestor(root) else {
        return false;
    };
    let (Ok(ancestor_meta), Ok(base_meta)) = (fs::symlink_metadata(&ancestor), fs::metadata(base))
    else {
        return false;
    };
    if !ancestor_meta.is_dir() || ancestor_meta.dev() != base_meta.dev() {
        return false;
    }
    ancestor
        .parent()
        .and_then(|parent| fs::metadata(parent).ok())
        .is_some_and(|parent| parent.dev() == ancestor_meta.dev())
}

fn nearest_existing_ancestor(root: &Path) -> Option<PathBuf> {
    root.ancestors()
        .skip(1)
        .find(|ancestor| fs::symlink_metadata(ancestor).is_ok())
        .map(Path::to_owned)
}
