//! Preserve only identities present in the final prepared JSON response.
use crate::diagnostics::{Outcome, RequestEvent};
use crate::{
    identity::ResultHandle,
    results::{self, HistoricalSource, ResultEntry},
    store::{Store, encode_path},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Immutable result lookup used by the final emission boundary.
pub(super) struct SavedEntries {
    root: PathBuf,
    singleton: Option<Store>,
    workspace: Option<Connection>,
}

impl SavedEntries {
    pub(super) fn singleton(root: &Path, cache: &Path) -> Self {
        let singleton = cache
            .join("index.sqlite3")
            .is_file()
            .then(|| Store::open(root, cache).ok())
            .flatten();
        Self {
            root: root.to_owned(),
            singleton,
            workspace: None,
        }
    }

    pub(super) fn workspace(root: &Path, cache: &Path) -> Self {
        let workspace = cache
            .join("workspace.sqlite3")
            .is_file()
            .then(|| Connection::open(cache.join("workspace.sqlite3")).ok())
            .flatten();
        Self {
            root: root.to_owned(),
            singleton: None,
            workspace,
        }
    }

    fn resolve(&self, handle: &str) -> Option<ResolvedEntry> {
        let parsed: ResultHandle = handle.parse().ok()?;
        if let Some(connection) = &self.workspace {
            if let Some(resolved) = resolve_workspace(connection, &parsed) {
                return Some(resolved);
            }
        }
        let store = self.singleton.as_ref()?;
        let (_, entry) = results::entry(store, handle).ok()?;
        Some(ResolvedEntry {
            owner_root: encode_path(&self.root),
            member: None,
            member_rank: parsed.ordinal,
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

fn resolve_workspace(connection: &Connection, handle: &ResultHandle) -> Option<ResolvedEntry> {
    let payload: Option<String> = connection
        .query_row(
            "SELECT payload FROM workspace_results WHERE id=?1 AND expires>?2",
            params![handle.set.simple().to_string(), results::now()],
            |row| row.get(0),
        )
        .optional()
        .ok()?
        .flatten();
    let payload: WorkspacePayload = serde_json::from_str(&payload?).ok()?;
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
    let Ok(value) = serde_json::from_str::<Value>(response) else {
        // A lines page names its handles in the first column; coverage is not captured.
        for (rank, handle) in crate::output::lines::emitted_handles(response)
            .enumerate()
            .take(128)
        {
            if let Some(identity) = resolved_handle(saved, handle, false, Some(rank + 1)) {
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
        if let Some(identity) = resolved_identity(saved, hit, false, Some(rank + 1)) {
            event.emitted.push(identity);
        }
    }
    if let Some(hit) = value.get("hit") {
        if let Some(identity) = resolved_identity(saved, hit, false, None) {
            event.emitted.push(identity);
        }
    }
    if let Some(edges) = value["relationships"].as_array() {
        if edges.len() > 50 {
            event.truncated = true;
        }
        for edge in edges.iter().take(50) {
            for side in ["source", "target"] {
                if let Some(identity) = resolved_identity(saved, &edge[side], false, None) {
                    event.emitted.push(identity);
                }
            }
        }
    }
    if values.len() > 64 {
        event.truncated = true;
    }
    for hit in values.iter().take(64) {
        if let Some(identity) = resolved_identity(saved, &hit["target"], false, None) {
            event.emitted.push(identity);
        }
        if let Some(candidates) = hit["candidates"].as_array() {
            if candidates.len() > 32 {
                event.truncated = true;
            }
            for candidate in candidates.iter().take(32) {
                if let Some(identity) = resolved_identity(saved, candidate, false, None) {
                    event.emitted.push(identity);
                }
            }
        }
        for side in ["before", "after"] {
            if let Some(identity) = resolved_side(saved, hit, side, false) {
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
                        if let Some(mut identity) = resolved_side(saved, hit, side, true) {
                            identity.start_byte = hunk[side]["start"]
                                .as_u64()
                                .unwrap_or(identity.start_byte as u64)
                                as usize;
                            identity.end_byte = hunk[side]["end"]
                                .as_u64()
                                .unwrap_or(identity.end_byte as u64)
                                as usize;
                            event.emitted.push(identity);
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
            .and_then(|handle| resolved_handle(saved, handle, true, None))
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
    saved: Option<&SavedEntries>,
    value: &Value,
    source_body: bool,
    original_rank: Option<usize>,
) -> Option<crate::diagnostics::EmittedIdentity> {
    if value["path"].as_str().is_none() && value["file"].as_str().is_none() {
        return None;
    }
    let handle = value["handle"].as_str()?;
    resolved_handle(saved, handle, source_body, original_rank)
}

fn resolved_handle(
    saved: Option<&SavedEntries>,
    handle: &str,
    source_body: bool,
    original_rank: Option<usize>,
) -> Option<crate::diagnostics::EmittedIdentity> {
    let saved = saved?;
    let resolved = saved.resolve(handle)?;
    identity_from_entry(&resolved, source_body, original_rank)
}

fn resolved_side(
    saved: Option<&SavedEntries>,
    hit: &Value,
    side: &str,
    source_body: bool,
) -> Option<crate::diagnostics::EmittedIdentity> {
    let handle = hit["handle"].as_str()?;
    if !hit[side].is_object() {
        return None;
    }
    let saved = saved?;
    let resolved = saved.resolve(handle)?;
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
mod tests {
    use super::*;
    use crate::diagnostics::{Operation, RequestContext};
    use serde_json::json;

    #[test]
    fn diff_and_later_pages_preserve_preimage_identity() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path(), cache.path()).unwrap();
        let oid = crate::identity::GitOid::parse(&"a".repeat(40)).unwrap();
        let revision = crate::identity::ContentRevision::of(b"historical source");
        let id = results::save_entries(
            &store,
            1,
            json!({}),
            vec![ResultEntry::Change(crate::results::ChangeEntry {
                handle: String::new(),
                name: "alpha".into(),
                status: "modified".into(),
                before: Some(HistoricalSource {
                    repository: encode_path(&root.path().join(".git")),
                    commit: oid,
                    blob: oid,
                    revision,
                    path: "a.rs".into(),
                    span: crate::identity::ByteSpan::new(10, 40).unwrap(),
                }),
                after: None,
                correspondence: "exact".into(),
            })],
            false,
        )
        .unwrap();
        let handle = format!("{id}:1");
        let source = json!({"repository":"/repo/.git","path":"a.rs","revision":revision,"commit":oid,"blob":oid,"span":{"start":10,"end":40}});
        let response = json!({"hits":[{"entry":"change","handle":handle,"before":source}],"hunks":[{"handle":handle,"before":{"start":12,"end":18,"text":"secret source"}}]});
        let mut event = RequestEvent::new(
            RequestContext::new(None, None),
            Operation::Diff,
            Outcome::Success,
        );
        capture_emitted(
            root.path(),
            &response.to_string(),
            &mut event,
            Some(&SavedEntries::singleton(root.path(), cache.path())),
            None,
        );
        assert_eq!(event.emitted.len(), 2);
        assert_eq!(event.emitted[0].original_rank, Some(1));
        assert_eq!(event.emitted[1].content_revision, revision.to_string());
        assert_eq!(
            (event.emitted[1].start_byte, event.emitted[1].end_byte),
            (12, 18)
        );
        assert!(event.emitted[1].source_body);
        assert_eq!(event.emitted[1].blob.as_deref(), Some(oid.as_str()));
        assert!(
            !serde_json::to_string(&event)
                .unwrap()
                .contains("secret source")
        );
    }

    #[test]
    fn candidate_metadata_and_capture_loss_are_explicit() {
        let candidates: Vec<_> = (0..40)
            .map(|n| json!({"path":"a.rs","revision":"rev","start":n,"end":n+1}))
            .collect();
        let response = json!({"hits":[{"path":"b.rs","revision":"rev2","start":0,"end":10,"candidates":candidates}]});
        let mut event = RequestEvent::new(
            RequestContext::new(None, None),
            Operation::References,
            Outcome::Success,
        );
        capture_emitted(
            std::path::Path::new("/repo"),
            &response.to_string(),
            &mut event,
            None,
            None,
        );
        assert!(event.emitted.is_empty());
    }

    #[test]
    fn saved_handles_supply_immutable_identity_and_unresolved_handles_are_ignored() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("lib.rs"), "fn alpha() {}\n").unwrap();
        let mut store = crate::store::Store::open(root.path(), cache.path()).unwrap();
        store.index().unwrap();
        let revision = crate::identity::ContentRevision::of(b"fn alpha() {}\n").to_string();
        let id = results::save_entries(
            &store,
            1,
            json!({}),
            vec![ResultEntry::LiveSource(crate::results::Hit {
                handle: String::new(),
                path: "lib.rs".into(),
                revision: Some(revision.clone()),
                start: 3,
                end: 8,
                start_line: 1,
                end_line: 1,
                name: "alpha".into(),
                kind: "function".into(),
                container: None,
                provenance: Some("exact_identifier".into()),
                resolution: None,
                candidates: Vec::new(),
                target: None,
            })],
            false,
        )
        .unwrap();
        let saved = SavedEntries::singleton(root.path(), cache.path());
        let mut event = RequestEvent::new(
            RequestContext::new(None, None),
            Operation::Search,
            Outcome::Success,
        );
        let response = json!({
            "hits":[
                {"handle":format!("{id}:1"),"file":"file:///wrong.rs","start_line":99,"end_line":99},
                {"handle":"0123456789abcdef0123456789abcdef:1","file":"file:///unpersisted.rs","start_line":1,"end_line":1}
            ]
        });
        capture_emitted(
            root.path(),
            &response.to_string(),
            &mut event,
            Some(&saved),
            None,
        );
        assert_eq!(event.emitted.len(), 1);
        assert_eq!(event.emitted[0].path, "lib.rs");
        assert_eq!(event.emitted[0].content_revision, revision);
        assert_eq!(
            (event.emitted[0].start_byte, event.emitted[0].end_byte),
            (3, 8)
        );
    }
}
