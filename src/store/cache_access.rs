//! Selected-cache errors retain their cause and carry an explicit refusal marker.
use crate::daemon::deadline::{QueryDeadline, TIMED_OUT};
use anyhow::{Result, bail};
use rusqlite::{Error as SqliteError, ErrorCode};
use std::{
    error::Error,
    fmt,
    fs::{self, OpenOptions},
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug)]
struct SelectedCacheRefusal;

impl fmt::Display for SelectedCacheRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("selected result cache is unusable")
    }
}

impl Error for SelectedCacheRefusal {}

/// Whether an error carries an explicit refusal for the selected cache.
pub fn is_index_cache_unavailable(error: &anyhow::Error) -> bool {
    !has_deadline_precedence(error) && has_selected_cache_marker(error)
}

/// Adds the selected cache and recovery options only for a cache failure.
pub fn index_cache_unavailable(cache: &Path, error: anyhow::Error) -> anyhow::Error {
    if has_deadline_precedence(&error)
        || error
            .chain()
            .any(|cause| cause.to_string().starts_with("index_cache_unavailable:"))
    {
        return error;
    }
    let marked = if has_selected_cache_marker(&error) {
        error
    } else if cache_refusal_cause(&error) {
        error.context(SelectedCacheRefusal)
    } else {
        return error;
    };
    marked.context(format!(
        "index_cache_unavailable: selected cache {} is unavailable; choose a writable path with `--cache` or set `XDG_CACHE_HOME` to a writable directory",
        cache.display()
    ))
}

fn has_selected_cache_marker(error: &anyhow::Error) -> bool {
    error.downcast_ref::<SelectedCacheRefusal>().is_some()
}

/// Checks that the selected result directory and an existing database are writable.
pub fn preflight_results_cache(cache: &Path, deadline: Option<&QueryDeadline>) -> Result<()> {
    check_deadline(deadline)?;
    if let Err(error) = fs::create_dir_all(cache) {
        return cache_io_error(cache, error, deadline);
    }
    check_deadline(deadline)?;

    let database = cache.join("results.sqlite3");
    match fs::metadata(&database) {
        Ok(metadata) if !metadata.is_file() => {
            check_deadline(deadline)?;
            let error = anyhow::anyhow!(
                "selected results database has invalid shape: {}",
                database.display()
            )
            .context(SelectedCacheRefusal);
            return Err(index_cache_unavailable(cache, error));
        }
        Ok(_) => {
            if let Err(error) = OpenOptions::new().read(true).write(true).open(&database) {
                return cache_io_error(cache, error, deadline);
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return cache_io_error(cache, error, deadline),
    }
    check_deadline(deadline)?;

    let probe = match create_probe(cache) {
        Ok(probe) => probe,
        Err(error) => return cache_io_error(cache, error, deadline),
    };
    let removal = fs::remove_file(probe);
    check_deadline(deadline)?;
    if let Err(error) = removal {
        return cache_io_error(cache, error, deadline);
    }
    check_deadline(deadline)
}

fn create_probe(cache: &Path) -> io::Result<PathBuf> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for attempt in 0..4 {
        let path = cache.join(format!(
            ".results-cache-probe-{}-{timestamp}-{attempt}",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                drop(file);
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Other,
        "could not choose a unique result-cache probe path",
    ))
}

fn cache_io_error<T>(
    cache: &Path,
    error: io::Error,
    deadline: Option<&QueryDeadline>,
) -> Result<T> {
    check_deadline(deadline)?;
    Err(index_cache_unavailable(cache, error.into()))
}

fn check_deadline(deadline: Option<&QueryDeadline>) -> Result<()> {
    if deadline.is_some_and(QueryDeadline::expired) {
        bail!("{TIMED_OUT}: query deadline expired");
    }
    Ok(())
}

fn has_deadline_precedence(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        let text = cause.to_string();
        text.starts_with("timed_out:")
            || text.starts_with("cancelled:")
            || text.starts_with("canceled:")
    })
}

fn cache_refusal_cause(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(cache_refusal_io)
            || cause.downcast_ref::<SqliteError>().is_some_and(|error| {
                // Scoped by the caller: only `index_cache_unavailable` reaches
                // here, with the cache the failed open targeted.
                matches!(
                    error,
                    SqliteError::SqliteFailure(failure, _)
                        if failure.code == ErrorCode::ReadOnly
                            || failure.code == ErrorCode::CannotOpen
                )
            })
    })
}

fn cache_refusal_io(error: &io::Error) -> bool {
    let kinds_match = matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied
            | io::ErrorKind::ReadOnlyFilesystem
            | io::ErrorKind::NotADirectory
            | io::ErrorKind::AlreadyExists
    );
    error.raw_os_error().map_or(kinds_match, |code| {
        matches!(
            code,
            libc::EACCES | libc::EPERM | libc::EROFS | libc::ENOTDIR | libc::EEXIST
        )
    })
}

#[cfg(test)]
mod tests;
