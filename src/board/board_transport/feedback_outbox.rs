//! Durable feedback queue; files survive router restarts until database commit.

mod board_spool;

use crate::board::BoardBackend;
use crate::board::board_protocol::{
    BOARD_API, BoardError, BoardErrorCode, BoardOp, BoardReply, BoardRequest, BoardResult,
};
use crate::board::board_vocabulary::FeedbackImportKey;
use board_spool::{private_directory, publish_atomic, sync_directory};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
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
        match backend.import_feedback(&request) {
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
    let mut record: QueuedFeedback = serde_json::from_slice(&bytes).map_err(json_error)?;
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
    // Only stored API 1 feedback upgrades to API 2; wire validation stays strict.
    if record.request.api == 1 && BOARD_API == 2 {
        record.request.api = BOARD_API;
    }
    record.request.validate().map_err(BoardError::from)?;
    Ok(record.request)
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
    match fs::rename(path, &destination) {
        Ok(()) => {
            use std::os::unix::ffi::OsStrExt;
            let name = std::ffi::CString::new(destination.as_os_str().as_bytes())
                .map_err(|_| invalid_record("quarantine path contains NUL"))?;
            // SAFETY: the owned CString is NUL terminated; null times requests now.
            // AT_SYMLINK_NOFOLLOW updates a quarantined link itself, never its target.
            let status = unsafe {
                libc::utimensat(
                    libc::AT_FDCWD,
                    name.as_ptr(),
                    std::ptr::null(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if status != 0 {
                return Err(io_error(std::io::Error::last_os_error()));
            }
            sync_directory(parent)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(error)),
    }
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
