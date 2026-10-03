//! Durable feedback queue; files survive router restarts until database commit.

use super::BoardBackend;
use super::board_protocol::{
    BoardError, BoardErrorCode, BoardOp, BoardReply, BoardRequest, BoardResult,
};
use super::board_vocabulary::FeedbackImportKey;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const OUTBOX_LIMIT: u64 = 64 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueuedFeedback {
    import_key: FeedbackImportKey,
    request: BoardRequest,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ImportSummary {
    pub imported: usize,
    pub quarantined: usize,
    pub pending: usize,
    pub retry_error: Option<BoardErrorCode>,
}

pub fn new_import_key() -> FeedbackImportKey {
    FeedbackImportKey::new()
}

/// Publishes a complete private record and acknowledges only durable writes.
pub fn queue(spool: &Path, request: &BoardRequest) -> Result<BoardReply, BoardError> {
    request.validate().map_err(BoardError::from)?;
    let mut request = request.clone();
    let BoardOp::Feedback { import_key, .. } = &mut request.op else {
        return Err(BoardError::new(
            BoardErrorCode::InvalidOptions,
            "only feedback can be queued",
        ));
    };
    let key = import_key.get_or_insert_with(new_import_key).clone();
    let record = QueuedFeedback {
        import_key: key.clone(),
        request,
    };
    let bytes = serde_json::to_vec(&record).map_err(json_error)?;
    if bytes.len() as u64 > OUTBOX_LIMIT {
        return Err(BoardError::new(
            BoardErrorCode::InvalidBody,
            "feedback frame exceeds 65536 bytes",
        ));
    }
    private_directory(spool)?;
    let path = spool.join(format!("{key}.feedback"));
    publish_atomic(&path, &bytes)?;
    Ok(BoardReply::new(
        "outbox",
        BoardResult::Queued { import_key: key },
    ))
}

/// Imports without claiming files; UUID transactions make concurrent replay safe.
pub fn import_pending(
    spool: &Path,
    backend: &mut dyn BoardBackend,
) -> Result<ImportSummary, BoardError> {
    let entries = match fs::read_dir(spool) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ImportSummary::default());
        }
        Err(error) => return Err(io_error(error)),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let path = entry.map_err(io_error)?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "feedback")
        {
            paths.push(path);
        }
    }
    paths.sort();
    let mut summary = ImportSummary::default();
    for path in paths {
        let request = match read_record(&path) {
            Ok(request) => request,
            Err(error) if error.code == BoardErrorCode::BoardUnavailable => {
                summary.pending += 1;
                summary.retry_error.get_or_insert(error.code);
                continue;
            }
            Err(_) => {
                quarantine(&path)?;
                summary.quarantined += 1;
                continue;
            }
        };
        match backend.handle(&request) {
            Ok(reply) if matches!(reply.result, BoardResult::Change(_)) => {
                match fs::remove_file(&path) {
                    Ok(()) => sync_directory(spool)?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(io_error(error)),
                }
                summary.imported += 1;
            }
            Err(error)
                if matches!(
                    error.code,
                    BoardErrorCode::BoardUnavailable | BoardErrorCode::DatabaseLocked
                ) =>
            {
                summary.pending += 1;
                summary.retry_error.get_or_insert(error.code);
            }
            Ok(_) | Err(_) => {
                quarantine(&path)?;
                summary.quarantined += 1;
            }
        }
    }
    Ok(summary)
}

fn read_record(path: &Path) -> Result<BoardRequest, BoardError> {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| invalid_record("invalid feedback filename"))?;
    let key = FeedbackImportKey::parse(stem).map_err(BoardError::from)?;
    let metadata = path.symlink_metadata().map_err(io_error)?;
    if !metadata.file_type().is_file() || metadata.len() > OUTBOX_LIMIT {
        return Err(invalid_record("feedback is not a bounded regular file"));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(io_error)?;
    if !file.metadata().map_err(io_error)?.file_type().is_file() {
        return Err(invalid_record("feedback changed into a non-regular file"));
    }
    file.take(OUTBOX_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() as u64 > OUTBOX_LIMIT {
        return Err(invalid_record("feedback exceeds frame limit"));
    }
    let record: QueuedFeedback = serde_json::from_slice(&bytes).map_err(json_error)?;
    if record.import_key != key {
        return Err(invalid_record("feedback UUID differs from filename"));
    }
    match &record.request.op {
        BoardOp::Feedback {
            import_key: Some(key),
            ..
        } if *key == record.import_key => {}
        _ => {
            return Err(invalid_record(
                "outbox requires feedback with its stable UUID",
            ));
        }
    }
    record.request.validate().map_err(BoardError::from)?;
    Ok(record.request)
}

fn publish_atomic(path: &Path, bytes: &[u8]) -> Result<(), BoardError> {
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

fn quarantine(path: &Path) -> Result<(), BoardError> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid_record("outbox has no directory"))?;
    let name = path
        .file_name()
        .ok_or_else(|| invalid_record("outbox has no filename"))?;
    let mut name = name.to_os_string();
    name.push(format!(".{}.quarantine", new_import_key()));
    let destination: PathBuf = parent.join(name);
    match fs::rename(path, destination) {
        Ok(()) => sync_directory(parent),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}

fn private_directory(path: &Path) -> Result<(), BoardError> {
    fs::create_dir_all(path).map_err(io_error)?;
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
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(io_error)
}

fn sync_directory(path: &Path) -> Result<(), BoardError> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(io_error)
}

fn invalid_record(message: impl Into<String>) -> BoardError {
    BoardError::new(BoardErrorCode::InvalidOptions, message)
}
fn io_error(error: std::io::Error) -> BoardError {
    BoardError::new(
        BoardErrorCode::BoardUnavailable,
        format!("feedback outbox: {error}"),
    )
}
fn json_error(error: serde_json::Error) -> BoardError {
    BoardError::new(
        BoardErrorCode::InvalidOptions,
        format!("feedback record: {error}"),
    )
}

#[cfg(test)]
mod tests;
