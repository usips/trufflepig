//! Private spool creation and atomic feedback record publication.

use super::{QueuedFeedback, invalid_record, io_error, json_error, new_import_key, read_record};
use crate::board::board_protocol::BoardError;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
};

pub(super) fn publish_atomic(path: &Path, bytes: &[u8]) -> Result<(), BoardError> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid_record("outbox has no directory"))?;
    let temporary = parent.join(format!("{}.feedback.pending", new_import_key()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)
            .map_err(io_error)?;
        file.write_all(bytes).map_err(io_error)?;
        file.sync_all().map_err(io_error)?;
        match fs::hard_link(&temporary, path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let old = read_record(path)?;
                let new: QueuedFeedback = serde_json::from_slice(bytes).map_err(json_error)?;
                if old != new.request {
                    return Err(invalid_record(
                        "queued UUID already names different feedback",
                    ));
                }
            }
            Err(error) => return Err(io_error(error)),
        }
        sync_directory(parent)
    })();
    let _ = fs::remove_file(&temporary);
    result
}

pub(super) fn private_directory(path: &Path) -> Result<(), BoardError> {
    // SAFETY: getuid has no preconditions and cannot fail.
    private_directory_owned_by(path, unsafe { libc::getuid() })
}

pub(super) fn private_directory_owned_by(path: &Path, owner: u32) -> Result<(), BoardError> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(io_error)?;
    if !path
        .symlink_metadata()
        .map_err(io_error)?
        .file_type()
        .is_dir()
    {
        return Err(invalid_record(
            "spool must be a directory rather than a symbolic link",
        ));
    }
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(io_error)?;
    let metadata = directory.metadata().map_err(io_error)?;
    if metadata.uid() != owner {
        return Err(invalid_record("spool must be owned by the current user"));
    }
    directory
        .set_permissions(fs::Permissions::from_mode(0o700))
        .map_err(io_error)
}

pub(super) fn sync_directory(path: &Path) -> Result<(), BoardError> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(io_error)
}
