use super::*;
use crate::{
    identity::ByteSpan,
    results::{ChangeEntry, HistoricalSource},
};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::os::unix::ffi::OsStrExt;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct TreeFile {
    pub path: String,
    pub oid: GitOid,
    pub mode: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct FileChange {
    pub before: Option<TreeFile>,
    pub after: Option<TreeFile>,
    pub status: String,
}

pub(super) fn tree(
    history: &History,
    revision: Option<&GitOid>,
) -> Result<BTreeMap<String, TreeFile>> {
    Ok(tree_unscoped(history, revision)?
        .into_values()
        .filter_map(|file| scope_file(history, file))
        .map(|file| (file.path.clone(), file))
        .collect())
}

fn scope_file(history: &History, mut file: TreeFile) -> Option<TreeFile> {
    let prefix = crate::store::encode_path(Path::new(&history.repository.root_prefix));
    file.path = file.path.strip_prefix(&prefix)?.to_owned();
    Some(file)
}

fn tree_unscoped(
    history: &History,
    revision: Option<&GitOid>,
) -> Result<BTreeMap<String, TreeFile>> {
    let Some(revision) = revision else {
        return Ok(BTreeMap::new());
    };
    let data =
        history
            .repository
            .run(&["ls-tree", "-rz", "--full-tree", revision.as_str(), "--"])?;
    let mut entries = BTreeMap::new();
    for row in data.split(|&b| b == 0).filter(|r| !r.is_empty()) {
        let split = row
            .iter()
            .position(|&b| b == b'\t')
            .context("history_unavailable: malformed tree record")?;
        let fields = std::str::from_utf8(&row[..split])?
            .split_whitespace()
            .collect::<Vec<_>>();
        ensure!(
            fields.len() == 3,
            "history_unavailable: malformed tree entry"
        );
        let raw = &row[split + 1..];
        let path = crate::store::encode_path(Path::new(std::ffi::OsStr::from_bytes(raw)));
        entries.insert(
            path.clone(),
            TreeFile {
                path,
                oid: GitOid::parse(fields[2])?,
                mode: fields[0].into(),
            },
        );
    }
    Ok(entries)
}

pub(super) fn compare(
    history: &History,
    before: Option<&GitOid>,
    after: &GitOid,
) -> Result<Vec<FileChange>> {
    let before_key = before.map(GitOid::as_str).unwrap_or("");
    // Cached facts are repository-wide only for full roots; subtree membership stays local.
    if history.repository.root_prefix.is_empty() {
        use rusqlite::OptionalExtension;
        let cached: Option<String> = history
            .conn
            .query_row(
                "SELECT payload FROM changes WHERE before_oid=?1 AND after_oid=?2",
                rusqlite::params![before_key, after.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(payload) = cached {
            return Ok(serde_json::from_str(&payload)?);
        }
    }
    let old = tree_unscoped(history, before)?;
    let new = tree_unscoped(history, Some(after))?;
    let mut deleted = Vec::new();
    let mut added = Vec::new();
    let mut changes = Vec::with_capacity(old.len().min(128));
    for (path, file) in &old {
        match new.get(path) {
            Some(next) if next.oid == file.oid && next.mode == file.mode => {}
            Some(next) => changes.push(FileChange {
                before: Some(file.clone()),
                after: Some(next.clone()),
                status: "modified".into(),
            }),
            None => deleted.push(file.clone()),
        }
    }
    for (path, file) in &new {
        if !old.contains_key(path) {
            added.push(file.clone());
        }
    }
    let mut old_counts = HashMap::new();
    let mut new_counts = HashMap::new();
    for file in &deleted {
        *old_counts.entry(file.oid).or_insert(0usize) += 1;
    }
    for file in &added {
        *new_counts.entry(file.oid).or_insert(0usize) += 1;
    }
    for file in deleted {
        let rename = if old_counts[&file.oid] == 1 && new_counts.get(&file.oid) == Some(&1) {
            added
                .iter()
                .position(|a| a.oid == file.oid && a.mode == file.mode)
        } else {
            None
        };
        if let Some(index) = rename {
            changes.push(FileChange {
                before: Some(file),
                after: Some(added.swap_remove(index)),
                status: "renamed_exact_blob".into(),
            });
        } else {
            changes.push(FileChange {
                before: Some(file),
                after: None,
                status: "deleted".into(),
            });
        }
    }
    changes.extend(added.into_iter().map(|file| FileChange {
        before: None,
        after: Some(file),
        status: "added".into(),
    }));
    changes.sort_by(|a, b| {
        a.after
            .as_ref()
            .or(a.before.as_ref())
            .map(|f| &f.path)
            .cmp(&b.after.as_ref().or(b.before.as_ref()).map(|f| &f.path))
    });
    if history.repository.root_prefix.is_empty() {
        let payload = serde_json::to_string(&changes)?;
        let _writer = storage::write_lease(&history.cache)?;
        if payload.len() <= 8 * 1024 * 1024 && storage::capacity(history).is_ok() {
            history.conn.execute(
                "INSERT OR IGNORE INTO changes VALUES(?1,?2,?3)",
                rusqlite::params![before_key, after.as_str(), payload],
            )?;
        }
    }
    if !history.repository.root_prefix.is_empty() {
        changes = changes
            .into_iter()
            .filter_map(|change| {
                let before = change.before.and_then(|f| scope_file(history, f));
                let after = change.after.and_then(|f| scope_file(history, f));
                if before.is_none() && after.is_none() {
                    return None;
                }
                let status = if change.status == "renamed_exact_blob"
                    && (before.is_none() || after.is_none())
                {
                    if before.is_none() {
                        "scope_boundary_added".into()
                    } else {
                        "scope_boundary_deleted".into()
                    }
                } else {
                    change.status
                };
                Some(FileChange {
                    before,
                    after,
                    status,
                })
            })
            .collect();
    }
    Ok(changes)
}

pub(super) fn source(
    history: &History,
    revision: &GitOid,
    file: &TreeFile,
) -> Result<HistoricalSource> {
    ensure!(
        matches!(file.mode.as_str(), "100644" | "100755"),
        "history_source_excluded: symlink or gitlink"
    );
    let bytes = history.repository.blob(&file.oid)?;
    Ok(HistoricalSource {
        repository: crate::store::encode_path(&history.repository.common_dir),
        commit: *revision,
        blob: file.oid,
        revision: crate::identity::ContentRevision::of(&bytes),
        path: file.path.clone(),
        span: ByteSpan::new(0, bytes.len())?,
    })
}

pub(super) fn entry(
    history: &History,
    before: Option<&GitOid>,
    after: &GitOid,
    change: &FileChange,
) -> Result<ChangeEntry> {
    let old = change
        .before
        .as_ref()
        .zip(before)
        .map(|(f, r)| source(history, r, f))
        .transpose()?;
    let new = change
        .after
        .as_ref()
        .map(|f| source(history, after, f))
        .transpose()?;
    Ok(ChangeEntry {
        handle: String::new(),
        name: change
            .after
            .as_ref()
            .or(change.before.as_ref())
            .context("empty change")?
            .path
            .clone(),
        status: change.status.clone(),
        before: old,
        after: new,
        correspondence: if change.status == "renamed_exact_blob" {
            "unique_exact_blob"
        } else {
            "path"
        }
        .into(),
    })
}
