//! Net comparisons read per-file original bytes from one published live generation.

use super::{History, comparison, source_diff, targets::Target};
use crate::{
    identity::{ByteSpan, ContentRevision, GitOid},
    output::OutputBudget,
    results::{self, ChangeEntry, HistoricalSource, Hit, ResultEntry},
    store::{PublicationChange, Store, encode_path},
};
use anyhow::{Result, ensure};
use rusqlite::OptionalExtension;
use serde_json::json;
use std::collections::BTreeMap;

const MAX_METADATA_BYTES: usize = 16 * 1024 * 1024;
const MAX_DIFF_LINES: usize = 100_000;

#[cfg(test)]
mod tests;

pub(super) fn since_uncommitted(
    history: &History,
    store: &mut Store,
    revision: &GitOid,
    target: Option<&str>,
    budget: &OutputBudget,
) -> Result<String> {
    store.index()?;
    let target = match super::commands::select(store, target, budget)? {
        Ok(target) => target,
        Err(response) => return Ok(response),
    };
    let old_tree = comparison::tree(history, Some(revision))?;
    let (entries, publication, excluded, truncated) = store.with_publication(|conn, publication| {
        let mut metadata_bytes: usize = old_tree.keys().map(|path| path.len() * 2 + 256).sum();
        ensure!(metadata_bytes <= MAX_METADATA_BYTES, "history_resource_limited: working tree metadata exceeds staging limit");
        let mut files = BTreeMap::new();
        let mut query = conn.prepare("SELECT path,revision,status FROM files ORDER BY path")?;
        for row in query.query_map([], |row| Ok((row.get::<_, String>(0)?, (row.get::<_, Option<String>>(1)?, row.get::<_, String>(2)?))))? {
            let (path, file) = row?;
            metadata_bytes += path.len() + file.0.as_ref().map_or(0, String::len) + file.1.len() + 256;
            ensure!(metadata_bytes <= MAX_METADATA_BYTES, "history_resource_limited: working tree metadata exceeds staging limit");
            files.insert(path, file);
        }
        let mut paths: Vec<_> = old_tree.keys().chain(files.keys()).collect();
        paths.sort();
        paths.dedup();
        let mut entries = Vec::with_capacity(paths.len().min(128));
        let mut excluded = 0;
        let mut truncated = false;
        for path in paths {
            if target.as_ref().is_some_and(|target| target.path != *path) { continue; }
            if entries.len() >= results::MAX_HITS { truncated = true; break; }
            let old_file = old_tree.get(path);
            let current = files.get(path);
            if old_file.is_some_and(|file| !matches!(file.mode.as_str(), "100644" | "100755")) {
                excluded += 1;
                entries.push(unavailable(path, "symlink_or_gitlink"));
                continue;
            }
            let old = match old_file.map(|file| history.repository.blob(&file.oid)).transpose() {
                Ok(bytes) => bytes,
                Err(_) => { excluded += 1; entries.push(unavailable(path, "historical_source_unavailable")); continue; }
            };
            let new = current.and_then(|(revision, _)| revision.as_ref()).map(|revision| {
                let length: i64 = conn.query_row("SELECT length(bytes) FROM contents WHERE revision=?1", [revision], |row| row.get(0))?;
                ensure!(length >= 0 && length <= super::MAX_BLOB_BYTES as i64, "history_resource_limited: working tree source exceeds blob limit");
                let bytes: Vec<u8> = conn.query_row("SELECT bytes FROM contents WHERE revision=?1", [revision], |row| row.get(0))?;
                ensure!(ContentRevision::of(&bytes) == ContentRevision::parse(revision)?, "working_tree_unavailable: published content identity mismatch");
                Ok::<_, anyhow::Error>(bytes)
            }).transpose()?;
            if let Some(expected) = target.as_ref().and_then(|target| target.revision.as_deref()) {
                ensure!(new.as_deref().is_some_and(|bytes| ContentRevision::of(bytes).to_string() == expected), "stale_handle: selected source differs from published revision");
            }
            if old.is_none() && new.is_none() && current.is_some() {
                excluded += 1;
                entries.push(unavailable(path, &current.expect("covered exclusion").1));
                continue;
            }
            if old == new { continue; }
            let lines = old.iter().chain(new.iter()).flat_map(|bytes| bytes.iter()).filter(|&&byte| byte == b'\n').take(MAX_DIFF_LINES + 1).count();
            if lines > MAX_DIFF_LINES {
                excluded += 1;
                entries.push(unavailable(path, "diff_line_resource_excluded"));
                continue;
            }
            let status = if current.is_some() && new.is_none() {
                "coverage_changed".to_owned()
            } else if current.is_none() {
                let observed: Option<String> = conn.query_row("SELECT value FROM publication_observations WHERE path=?1 ORDER BY generation DESC LIMIT 1", [path], |row| row.get(0)).optional()?;
                observed.map(|value| serde_json::from_str::<PublicationChange>(&value).map(|change| change.kind)).transpose()?.unwrap_or_else(|| "absent_from_published_coverage".into())
            } else if old.is_none() { "added".into() } else { "modified".into() };
            if matches!(status.as_str(), "coverage_changed" | "ignored_or_excluded" | "absent_from_published_coverage" | "coverage_unknown") { excluded += 1; }
            let (spans, spans_truncated) = changed_spans(path, old.as_deref(), new.as_deref(), target.as_ref())?;
            truncated |= spans_truncated;
            for (before, after, correspondence) in spans {
                if entries.len() + usize::from(before.is_some()) + usize::from(after.is_some()) > results::MAX_HITS { truncated = true; break; }
                if let Some(span) = before {
                    let file = old_file.expect("preimage span has tree file");
                    entries.push(ResultEntry::Change(ChangeEntry {
                        handle: String::new(), name: path.clone(), status: status.clone(),
                        before: Some(HistoricalSource { repository: encode_path(&history.repository.common_dir), commit: *revision, blob: file.oid, revision: ContentRevision::of(old.as_deref().expect("preimage content")), path: path.clone(), span }),
                        after: None, correspondence: correspondence.clone(),
                    }));
                }
                if let Some(span) = after {
                    let bytes = new.as_deref().expect("postimage span has content");
                    entries.push(ResultEntry::LiveSource(Hit {
                        handle: String::new(), path: path.clone(), revision: current.and_then(|(revision, _)| revision.clone()),
                        start: span.start, end: span.end,
                        start_line: line_number(bytes, span.start), end_line: line_number(bytes, span.end.saturating_sub(1).max(span.start)),
                        name: path.clone(), kind: "file".into(), container: None,
                        provenance: Some(format!("published_generation:{}:after", publication.generation)),
                        resolution: Some(correspondence), candidates: Vec::new(), target: None,
                    }));
                }
            }
        }
        Ok((entries, publication.clone(), excluded, truncated))
    })?;
    let id = results::save_entries(
        store,
        publication.generation,
        json!({
            "operation":"since", "before":revision, "captured_head":history.tip,
            "endpoint":"published_working_tree", "publication":publication,
            "comparison":"direct_net", "excluded":excluded,
            "source":"explicit_show_required", "absent_paths":"deletion_requires_published_observation",
        }),
        entries,
        truncated,
    )?;
    results::page(store, &id, 0, 20, budget)
}

type ChangedSpans = (Option<ByteSpan>, Option<ByteSpan>, String);

fn changed_spans(
    path: &str,
    old: Option<&[u8]>,
    new: Option<&[u8]>,
    target: Option<&Target>,
) -> Result<(Vec<ChangedSpans>, bool)> {
    let differences = source_diff::diff_sources(old.unwrap_or_default(), new.unwrap_or_default());
    if let Some(target) = target.filter(|target| target.symbol.is_some()) {
        let before = crate::extract::extract(path, old.unwrap_or_default());
        let after = crate::extract::extract(path, new.unwrap_or_default());
        let correspondence = source_diff::correspond_declarations(
            old.unwrap_or_default(),
            new.unwrap_or_default(),
            &before,
            &after,
            &differences,
        );
        let mut selected = Vec::with_capacity(correspondence.len().min(16));
        let mut truncated = false;
        for relation in correspondence {
            if relation.relation == source_diff::DeclarationRelation::Unchanged {
                continue;
            }
            let left = relation.before.map(|index| &before.definitions[index]);
            let right = relation.after.map(|index| &after.definitions[index]);
            if !left
                .into_iter()
                .chain(right)
                .any(|definition| Some(&definition.name) == target.symbol.as_ref())
            {
                continue;
            }
            // A selected occurrence remains distinct when its name has duplicates.
            if target.span.is_some_and(|span| {
                !left
                    .into_iter()
                    .chain(right)
                    .any(|definition| definition.start == span.start && definition.end == span.end)
            }) {
                continue;
            }
            let before = left
                .map(|definition| ByteSpan::new(definition.start, definition.end))
                .transpose()?;
            let after = right
                .map(|definition| ByteSpan::new(definition.start, definition.end))
                .transpose()?;
            if selected.len() == results::MAX_HITS {
                truncated = true;
                break;
            }
            selected.push((
                before,
                after,
                serde_json::to_value(relation.relation)?
                    .as_str()
                    .unwrap_or("uncertain")
                    .into(),
            ));
        }
        return Ok((selected, truncated));
    }
    if differences.changes.is_empty() {
        return Ok((
            vec![(
                old.map(|bytes| ByteSpan {
                    start: 0,
                    end: bytes.len(),
                }),
                new.map(|bytes| ByteSpan {
                    start: 0,
                    end: bytes.len(),
                }),
                "path".into(),
            )],
            false,
        ));
    }
    let truncated = differences.changes.len() > results::MAX_HITS;
    Ok((
        differences
            .changes
            .into_iter()
            .take(results::MAX_HITS)
            .map(|change| {
                (
                    old.map(|_| change.before),
                    new.map(|_| change.after),
                    "path".into(),
                )
            })
            .collect(),
        truncated,
    ))
}

fn line_number(bytes: &[u8], offset: usize) -> usize {
    bytes[..offset]
        .iter()
        .filter(|&&byte| byte == b'\n')
        .count()
        + 1
}

fn unavailable(path: &str, status: &str) -> ResultEntry {
    ResultEntry::Change(ChangeEntry {
        handle: String::new(),
        name: path.into(),
        status: status.into(),
        before: None,
        after: None,
        correspondence: "unavailable".into(),
    })
}
