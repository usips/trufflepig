//! Decides whether a missing root was deleted or may sit on an unmounted volume.
//! A root whose parent directory exists was deleted. Otherwise the root must lie
//! under the user's home, and its nearest existing ancestor must be a directory in
//! home on the cache base's device and not a mount point. Anything else (removable
//! media whose mount directory vanished, another device, a mount point, a file in
//! the way) may hide an unmounted volume, so its cache is kept.

use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

/// True when `root` is gone and the module rule proves it was deleted.
pub(super) fn root_deleted(root: &Path, base: &Path, home: Option<&Path>) -> bool {
    if fs::symlink_metadata(root).is_ok() {
        return false;
    }
    if root.parent().is_some_and(Path::is_dir) {
        return true;
    }
    let Some(home) = home.map(|home| home.canonicalize().unwrap_or_else(|_| home.to_owned()))
    else {
        return false;
    };
    let Some(ancestor) = nearest_existing_ancestor(root) else {
        return false;
    };
    if !root.starts_with(&home) || !ancestor.starts_with(&home) {
        return false;
    }
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
