//! Preserve only identities present in the final prepared JSON response.
use crate::diagnostics::{Outcome, RequestEvent};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tp_core::{identity::ResultHandle, path_codec::encode_path, query_deadline::QueryDeadline};
use tp_index::results::{self, HistoricalSource, RESULTS_DATABASE, ResultEntry, ResultSetStore};

const AUDIT_DEADLINE: Duration = Duration::from_millis(50);
const AUDIT_BUSY_SLICE: Duration = Duration::from_millis(25);
const AUDIT_CHECK_STEPS: i32 = 1_024;

/// Immutable result lookup used by the final emission boundary.
pub(super) struct SavedEntries {
    root: PathBuf,
    singleton: Option<ResultSetStore>,
    workspace: Option<Connection>,
    deadline: QueryDeadline,
}

impl SavedEntries {
    pub(super) fn singleton(root: &Path, cache: &Path) -> Self {
        let deadline = QueryDeadline::after(AUDIT_DEADLINE);
        let singleton = cache
            .join(RESULTS_DATABASE)
            .is_file()
            .then(|| ResultSetStore::open_with_deadline(cache, &deadline).ok())
            .flatten();
        Self {
            root: root.to_owned(),
            singleton,
            workspace: None,
            deadline,
        }
    }

    pub(super) fn workspace(root: &Path, cache: &Path) -> Self {
        let deadline = QueryDeadline::after(AUDIT_DEADLINE);
        let workspace = cache
            .join("workspace.sqlite3")
            .is_file()
            .then(|| Connection::open(cache.join("workspace.sqlite3")).ok())
            .flatten();
        let workspace = workspace.and_then(|connection| {
            let handler_deadline = deadline.clone();
            connection
                .progress_handler(AUDIT_CHECK_STEPS, Some(move || handler_deadline.expired()))
                .ok()
                .map(|()| connection)
        });
        Self {
            root: root.to_owned(),
            singleton: None,
            workspace,
            deadline,
        }
    }

    fn resolve(&self, handle: &ResultHandle) -> Option<ResolvedEntry> {
        if self.deadline.expired() {
            return None;
        }
        if let Some(connection) = &self.workspace {
            if let Some(resolved) = resolve_workspace(connection, handle, &self.deadline) {
                return Some(resolved);
            }
        }
        let set = self
            .singleton
            .as_ref()?
            .load(&handle.set.simple().to_string())
            .ok()?;
        let entry = set.hits.get(handle.ordinal.checked_sub(1)?)?.clone();
        Some(ResolvedEntry {
            owner_root: encode_path(&self.root),
            member: None,
            member_rank: handle.ordinal,
            entry,
        })
    }
}

struct ResolvedEntry {
    owner_root: String,
    member: Option<String>,
    member_rank: usize,
    entry: ResultEntry,
}

#[derive(serde::Deserialize)]
struct WorkspaceOwner {
    name: String,
    root: String,
}

#[derive(serde::Deserialize)]
struct WorkspaceOwnedEntry {
    owner: usize,
    member_rank: usize,
    entry: ResultEntry,
}

#[derive(serde::Deserialize)]
struct WorkspacePayload {
    owners: Vec<WorkspaceOwner>,
    hits: Vec<WorkspaceOwnedEntry>,
}

fn resolve_workspace(
    connection: &Connection,
    handle: &ResultHandle,
    deadline: &QueryDeadline,
) -> Option<ResolvedEntry> {
    if deadline.expired() {
        return None;
    }
    connection
        .busy_timeout(deadline.cap(AUDIT_BUSY_SLICE))
        .ok()?;
    let payload: Option<String> = connection
        .query_row(
            "SELECT payload FROM workspace_results WHERE id=?1 AND expires>?2",
            params![handle.set.simple().to_string(), results::now()],
            |row| row.get(0),
        )
        .optional()
        .ok()?
        .flatten();
    if deadline.expired() {
        return None;
    }
    let payload: WorkspacePayload = serde_json::from_str(&payload?).ok()?;
    if deadline.expired() {
        return None;
    }
    let owned = payload.hits.get(handle.ordinal.checked_sub(1)?)?;
    let owner = payload.owners.get(owned.owner)?;
    Some(ResolvedEntry {
        owner_root: owner.root.clone(),
        member: Some(owner.name.clone()),
        member_rank: owned.member_rank,
        entry: owned.entry.clone(),
    })
}

pub(super) fn capture_emitted(
    root: &Path,
    response: &str,
    event: &mut RequestEvent,
    saved: Option<&SavedEntries>,
    requested_target: Option<&str>,
) {
    let mut resolve = |handle: &ResultHandle| match saved {
        Some(saved) => saved.resolve(handle).ok_or_else(|| {
            if saved.deadline.expired() {
                LookupFailure::Expired
            } else {
                LookupFailure::Unavailable
            }
        }),
        None => Err(LookupFailure::Unavailable),
    };
    capture_emitted_with_resolver(root, response, event, requested_target, &mut resolve);
}

#[derive(Clone, Copy)]
enum LookupFailure {
    Expired,
    Unavailable,
}

fn capture_emitted_with_resolver(
    root: &Path,
    response: &str,
    event: &mut RequestEvent,
    requested_target: Option<&str>,
    resolve: &mut impl FnMut(&ResultHandle) -> Result<ResolvedEntry, LookupFailure>,
) {
    let Ok(value) = serde_json::from_str::<Value>(response) else {
        // A lines page names its handles in the first column; coverage is not captured.
        for (rank, handle) in tp_core::output::lines_format::emitted_handles(response)
            .enumerate()
            .take(129)
        {
            if rank == 128 {
                event.truncated = true;
                break;
            }
            if let Some(identity) = resolved_handle(resolve, event, handle, false, Some(rank + 1)) {
                event.emitted.push(identity);
            }
        }
        return;
    };
    let fallback_repository = encode_path(root);
    let repository = value["repository"].as_str().unwrap_or(&fallback_repository);
    event.coverage = value
        .get("coverage")
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    if value["hits"].as_array().is_some_and(Vec::is_empty) {
        event.outcome = Outcome::Empty;
    }
    let values = value
        .get("hits")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    for (rank, hit) in values.iter().enumerate().take(128) {
        if let Some(identity) = resolved_identity(resolve, event, hit, false, Some(rank + 1)) {
            event.emitted.push(identity);
        }
    }
    if let Some(hit) = value.get("hit") {
        if let Some(identity) = resolved_identity(resolve, event, hit, false, None) {
            event.emitted.push(identity);
        }
    }
    if let Some(edges) = value["relationships"].as_array() {
        if edges.len() > 50 {
            event.truncated = true;
        }
        for edge in edges.iter().take(50) {
            for side in ["source", "target"] {
                if let Some(identity) = resolved_identity(resolve, event, &edge[side], false, None)
                {
                    event.emitted.push(identity);
                }
            }
        }
    }
    if values.len() > 64 {
        event.truncated = true;
    }
    for hit in values.iter().take(64) {
        if let Some(identity) = resolved_identity(resolve, event, &hit["target"], false, None) {
            event.emitted.push(identity);
        }
        if let Some(candidates) = hit["candidates"].as_array() {
            if candidates.len() > 32 {
                event.truncated = true;
            }
            for candidate in candidates.iter().take(32) {
                if let Some(identity) = resolved_identity(resolve, event, candidate, false, None) {
                    event.emitted.push(identity);
                }
            }
        }
        for side in ["before", "after"] {
            if let Some(identity) = resolved_side(resolve, event, hit, side, false) {
                event.emitted.push(identity);
            }
        }
    }
    if let Some(hunks) = value["hunks"].as_array() {
        if hunks.len() > 64 {
            event.truncated = true;
        }
        for hunk in hunks.iter().take(64) {
            if let Some(hit) = values.iter().find(|hit| hit["handle"] == hunk["handle"]) {
                for side in ["before", "after"] {
                    if hunk[side]["text"].is_string() {
                        if let Some(mut identity) = resolved_side(resolve, event, hit, side, true) {
                            identity.start_byte = hunk[side]["start"]
                                .as_u64()
                                .unwrap_or(identity.start_byte as u64)
                                as usize;
                            identity.end_byte = hunk[side]["end"]
                                .as_u64()
                                .unwrap_or(identity.end_byte as u64)
                                as usize;
                            event.emitted.push(identity);
                        } else if !hit[side].is_object() {
                            // Hunk text has no identity without its source side.
                            event.truncated = true;
                        }
                    }
                }
            }
        }
    }
    if values.len() > 128 {
        event.truncated = true;
    }
    if event.emitted.len() > 128 {
        event.truncated = true;
        event.emitted.truncate(128);
    }
    if value.get("lines").is_some() {
        let requested = requested_target.and_then(base_handle);
        if let Some(mut identity) = requested
            .as_deref()
            .and_then(|handle| resolved_handle(resolve, event, handle, true, None))
            .or_else(|| {
                // Explicit current path reads have no input handle. Their
                // source body is still the final response's own identity.
                requested_target
                    .is_some_and(|target| base_handle(target).is_none())
                    .then(|| direct_identity(&repository, &value))
                    .flatten()
            })
        {
            if let Some(lines) = value["lines"].as_array() {
                identity.start_byte = lines
                    .first()
                    .and_then(|v| v["start"].as_u64())
                    .unwrap_or(identity.start_byte as u64)
                    as usize;
                identity.end_byte = lines
                    .last()
                    .and_then(|v| v["end"].as_u64())
                    .unwrap_or(identity.start_byte as u64)
                    as usize;
            }
            event.emitted.push(identity);
        }
    }
}

fn resolved_identity(
    resolve: &mut impl FnMut(&ResultHandle) -> Result<ResolvedEntry, LookupFailure>,
    event: &mut RequestEvent,
    value: &Value,
    source_body: bool,
    original_rank: Option<usize>,
) -> Option<crate::diagnostics::EmittedIdentity> {
    if value["path"].as_str().is_none() && value["file"].as_str().is_none() {
        return None;
    }
    let handle = value["handle"].as_str()?;
    resolved_handle(resolve, event, handle, source_body, original_rank)
}

fn resolved_handle(
    resolve: &mut impl FnMut(&ResultHandle) -> Result<ResolvedEntry, LookupFailure>,
    event: &mut RequestEvent,
    handle: &str,
    source_body: bool,
    original_rank: Option<usize>,
) -> Option<crate::diagnostics::EmittedIdentity> {
    let resolved = resolve_entry(resolve, event, handle)?;
    identity_from_entry(&resolved, source_body, original_rank)
}

fn resolved_side(
    resolve: &mut impl FnMut(&ResultHandle) -> Result<ResolvedEntry, LookupFailure>,
    event: &mut RequestEvent,
    hit: &Value,
    side: &str,
    source_body: bool,
) -> Option<crate::diagnostics::EmittedIdentity> {
    let handle = hit["handle"].as_str()?;
    if !hit[side].is_object() {
        return None;
    }
    let resolved = resolve_entry(resolve, event, handle)?;
    let ResultEntry::Change(change) = &resolved.entry else {
        return identity_from_entry(&resolved, source_body, None);
    };
    let source = match side {
        "before" => change.before.as_ref(),
        "after" => change.after.as_ref(),
        _ => None,
    }?;
    Some(identity_from_historical(
        &resolved,
        source,
        source_body,
        Some(resolved.member_rank),
    ))
}

fn resolve_entry(
    resolve: &mut impl FnMut(&ResultHandle) -> Result<ResolvedEntry, LookupFailure>,
    event: &mut RequestEvent,
    handle: &str,
) -> Option<ResolvedEntry> {
    let Ok(handle) = handle.parse::<ResultHandle>() else {
        return None;
    };
    match resolve(&handle) {
        Ok(resolved) => Some(resolved),
        Err(LookupFailure::Expired | LookupFailure::Unavailable) => {
            event.truncated = true;
            None
        }
    }
}

fn identity_from_entry(
    resolved: &ResolvedEntry,
    source_body: bool,
    original_rank: Option<usize>,
) -> Option<crate::diagnostics::EmittedIdentity> {
    match &resolved.entry {
        ResultEntry::LiveSource(hit) => Some(crate::diagnostics::EmittedIdentity {
            owner_root: Some(resolved.owner_root.clone()),
            member: resolved.member.clone(),
            repository: resolved.owner_root.clone(),
            path: hit.path.clone(),
            content_revision: hit.revision.clone().unwrap_or_default(),
            commit: None,
            blob: None,
            start_byte: hit.start,
            end_byte: hit.end,
            original_rank: Some(resolved.member_rank),
            source_body,
        }),
        ResultEntry::Change(change) => change
            .after
            .as_ref()
            .or(change.before.as_ref())
            .map(|source| identity_from_historical(resolved, source, source_body, original_rank)),
        ResultEntry::Commit(_) => None,
    }
}

fn identity_from_historical(
    resolved: &ResolvedEntry,
    source: &HistoricalSource,
    source_body: bool,
    _original_rank: Option<usize>,
) -> crate::diagnostics::EmittedIdentity {
    crate::diagnostics::EmittedIdentity {
        owner_root: Some(resolved.owner_root.clone()),
        member: resolved.member.clone(),
        repository: source.repository.clone(),
        path: source.path.clone(),
        content_revision: source.revision.to_string(),
        commit: Some(source.commit.to_string()),
        blob: Some(source.blob.to_string()),
        start_byte: source.span.start,
        end_byte: source.span.end,
        original_rank: Some(resolved.member_rank),
        source_body,
    }
}

fn base_handle(target: &str) -> Option<String> {
    let target = target.strip_prefix("read:").unwrap_or(target);
    let target = target.rsplit_once('@').map_or(target, |(handle, _)| handle);
    let target = target
        .rsplit_once(':')
        .filter(|(_, side)| matches!(*side, "before" | "after"))
        .map_or(target, |(handle, _)| handle);
    target.parse::<ResultHandle>().ok().map(|_| target.into())
}

fn direct_identity(repository: &str, value: &Value) -> Option<crate::diagnostics::EmittedIdentity> {
    Some(crate::diagnostics::EmittedIdentity {
        owner_root: Some(repository.into()),
        member: value["member"].as_str().map(str::to_owned),
        repository: value["repository"].as_str().unwrap_or(repository).into(),
        path: value["path"].as_str()?.into(),
        content_revision: value["revision"].as_str().unwrap_or_default().into(),
        commit: value["historical"]["commit"].as_str().map(str::to_owned),
        blob: value["historical"]["blob"].as_str().map(str::to_owned),
        start_byte: value["start"].as_u64().unwrap_or(0) as usize,
        end_byte: value["end"].as_u64().unwrap_or(0) as usize,
        original_rank: None,
        source_body: true,
    })
}

#[cfg(test)]
mod tests;
