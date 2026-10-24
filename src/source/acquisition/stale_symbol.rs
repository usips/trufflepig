//! `sym:` reads whose file changed after publication: wait briefly for a
//! serving daemon to republish, then locate the definition in current bytes.
use super::{AcquiredSource, acquire_entry};
use crate::{
    query_deadline::QueryDeadline,
    results::{self, Hit, ResultEntry},
    store::Store,
};
use anyhow::{Result, bail};
use rusqlite::OptionalExtension;
use std::{
    path::Path,
    thread,
    time::{Duration, Instant},
};

/// Longest a stale `sym:` read waits for a serving daemon to republish.
const REPUBLISH_WAIT: Duration = Duration::from_secs(3);
const REPUBLISH_POLL: Duration = Duration::from_millis(100);

/// A `sym:` read of `entry`. When its file changed since publication, a serving
/// daemon gets [`REPUBLISH_WAIT`] to republish; the definition is then located
/// in current bytes, verified only if the index now publishes those bytes and
/// otherwise returned unverified as `freshness: unpublished`.
pub(super) fn acquire_symbol_entry(
    store: &Store,
    handle: &str,
    entry: ResultEntry,
) -> Result<AcquiredSource> {
    let ResultEntry::LiveSource(hit) = &entry else {
        return acquire_entry(store, handle, entry, None);
    };
    let hit = hit.clone();
    let seen = store.generation()?;
    let error = match acquire_entry(store, handle, entry, None) {
        Err(error) if is_stale_source(&error) => error,
        read => return read,
    };
    let republished = await_republish(store, seen)?;
    // Binary or unextractable current bytes leave the stale answer standing.
    let mut source = match acquire_reextracted(store, &hit) {
        Ok(Some(source)) => source,
        Ok(None) => return Err(error),
        Err(unavailable) => bail!("{error}; current bytes: {unavailable:#}"),
    };
    if republished && published_revision(store, &hit.path)? == Some(source.revision.to_string()) {
        source.verified = true;
        source.freshness = None;
    }
    Ok(source)
}

/// The revision the current publication records for `path`.
fn published_revision(store: &Store, path: &str) -> Result<Option<String>> {
    Ok(store
        .conn
        .query_row("SELECT revision FROM files WHERE path=?1", [path], |row| {
            row.get::<_, Option<String>>(0)
        })
        .optional()?
        .flatten())
}

fn is_stale_source(error: &anyhow::Error) -> bool {
    crate::coded_error::ErrorCode::StaleSource.is_code_of(error)
}

/// Waits while a daemon serves this index for a generation after `seen`. A
/// worktree read through a parent's index is never fixed by a republish.
fn await_republish(store: &Store, seen: i64) -> Result<bool> {
    if store.root != store.index_root() {
        return Ok(false);
    }
    let serving = match store.query_deadline() {
        Some(deadline) => running_for_republish(store.index_cache(), deadline)?,
        None => crate::daemon::running(store.index_cache()),
    };
    if !serving {
        return Ok(false);
    }
    let started = Instant::now();
    loop {
        if let Some(deadline) = store.query_deadline() {
            check_query_deadline(deadline)?;
        }
        let remaining = REPUBLISH_WAIT.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Ok(false);
        }
        let wait = store
            .query_deadline()
            .map_or(REPUBLISH_POLL.min(remaining), |deadline| {
                deadline.cap(REPUBLISH_POLL.min(remaining))
            });
        if wait.is_zero() {
            return Err(query_timeout_error());
        }
        thread::sleep(wait);
        if let Some(deadline) = store.query_deadline() {
            check_query_deadline(deadline)?;
        }
        if store.generation()? > seen {
            return Ok(true);
        }
    }
}

/// Keeps the daemon liveness probe shorter than the query while preserving
/// `running`'s conservative treatment of a busy serving socket.
fn running_for_republish(cache: &Path, deadline: &QueryDeadline) -> Result<bool> {
    check_query_deadline(deadline)?;
    let probe_deadline = deadline.capped(crate::daemon::RUNNING_PROBE_WAIT);
    liveness_probe_result(crate::daemon::running_by(cache, &probe_deadline), deadline)
}

fn liveness_probe_result(result: Result<bool>, deadline: &QueryDeadline) -> Result<bool> {
    match result {
        Ok(serving) => {
            check_query_deadline(deadline)?;
            Ok(serving)
        }
        Err(error)
            if crate::coded_error::ErrorCode::TimedOut.is_code_of(&error)
                && !deadline.expired() =>
        {
            Ok(true)
        }
        Err(error) if crate::coded_error::ErrorCode::TimedOut.is_code_of(&error) => Err(error),
        Err(_) if deadline.expired() => Err(query_timeout_error()),
        Err(_) => Ok(false),
    }
}

fn check_query_deadline(deadline: &QueryDeadline) -> Result<()> {
    if deadline.expired() {
        return Err(query_timeout_error());
    }
    Ok(())
}

fn query_timeout_error() -> anyhow::Error {
    anyhow::anyhow!(coded!(TimedOut, "stale symbol republish wait"))
}

/// `hit`'s definition in its original container, located in the file's current
/// bytes and saved as its own result so continuations address those bytes.
fn acquire_reextracted(store: &Store, hit: &Hit) -> Result<Option<AcquiredSource>> {
    let Some(definition) = crate::source::reextract_definition(&store.root, hit)? else {
        return Ok(None);
    };
    let set = results::save_entries(
        store,
        store.generation()?,
        serde_json::json!({}),
        vec![ResultEntry::LiveSource(definition.hit())],
        false,
    )?;
    Ok(Some(AcquiredSource {
        path: definition.path,
        revision: definition.revision,
        span: definition.span,
        bytes: definition.bytes,
        verified: false,
        handle: format!("{set}:1"),
        side: None,
        historical: None,
        definitions: None,
        freshness: Some("unpublished"),
    }))
}

#[cfg(test)]
mod tests;
