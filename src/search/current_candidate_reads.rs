//! Verify published-index candidates against bounded current-source snapshots.
use super::{Query, search_current_file, semantic_representative};
use crate::{
    identity::ContentRevision,
    results::Hit,
    source,
    store::{Store, decode_path},
};
use anyhow::Result;
use std::collections::{HashMap, HashSet};

const MAX_CHECKED_FILES: usize = 128;
const MAX_REEXTRACTED_FILES: usize = 64;

/// Replaces stale candidates in rank order without changing the published index.
pub(super) fn recheck(
    store: &Store,
    query: &Query,
    hits: &mut Vec<Hit>,
    coverage: &mut serde_json::Value,
    truncated: &mut bool,
) -> Result<()> {
    let mut checked = HashSet::with_capacity(hits.len().min(MAX_CHECKED_FILES));
    let mut replacements = HashMap::with_capacity(MAX_REEXTRACTED_FILES.min(hits.len()));
    let mut unverified = HashSet::new();
    let mut reextracted = 0;
    let mut semantic_unverified = 0;
    for hit in hits.iter() {
        let Some(revision) = hit.revision.as_deref() else {
            continue;
        };
        if !checked.insert(hit.path.as_str()) {
            continue;
        }
        if checked.len() > MAX_CHECKED_FILES {
            unverified.insert(hit.path.as_str());
            continue;
        }
        let read = decode_path(&hit.path)
            .and_then(|path| source::read_contained(&store.root, &path, source::MAX_READ_BYTES));
        let bytes = match read {
            Ok(bytes) => bytes,
            Err(error) if missing(&error) => {
                replacements.insert(hit.path.as_str(), Vec::new());
                continue;
            }
            Err(_) => {
                unverified.insert(hit.path.as_str());
                continue;
            }
        };
        if ContentRevision::of(&bytes).to_string() == revision {
            continue;
        }
        if reextracted == MAX_REEXTRACTED_FILES || bytes.contains(&0) {
            unverified.insert(hit.path.as_str());
            continue;
        }
        reextracted += 1;
        let current =
            source::usable_extraction(&hit.path, crate::extract::extract(&hit.path, &bytes))
                .and_then(|extraction| {
                    let (mut current, incomplete) =
                        search_current_file(store, query, &hit.path, &bytes, &extraction)?;
                    let semantic = hits.iter().any(|candidate| {
                        candidate.path == hit.path
                            && candidate.provenance.as_deref() == Some("semantic")
                    });
                    if semantic {
                        semantic_unverified += 1;
                        if let Some(candidate) =
                            semantic_representative(query, &hit.path, &bytes, &extraction)
                        {
                            current.push(candidate);
                        }
                    }
                    Ok((current, incomplete))
                });
        match current {
            Ok((current, incomplete)) => {
                *truncated |= incomplete;
                replacements.insert(hit.path.as_str(), current);
            }
            Err(_) => {
                unverified.insert(hit.path.as_str());
            }
        }
    }
    let unsearched = unverified.len();
    if unsearched > 0 {
        *truncated = true;
        coverage["current_source_unsearched"] = unsearched.into();
        coverage["current_source_recheck"] =
            serde_json::json!({"status":"incomplete", "unsearched_paths":unsearched});
    }
    let semantic_currentness_unknown = matches!(
        coverage["semantic_status"].as_str(),
        Some("ready" | "partial")
    ) && (!replacements.is_empty() || unsearched > 0);
    if semantic_currentness_unknown {
        semantic_unverified = replacements.len() + unsearched;
    }
    if semantic_unverified > 0 {
        coverage["current_source_semantic"] =
            serde_json::json!({"status":"unverified", "files":semantic_unverified});
        coverage["semantic_status"] = "partial".into();
    }
    if (!replacements.is_empty() || unsearched > 0) && coverage["rerank_status"] == "ready" {
        coverage["rerank_status"] = "unavailable".into();
        coverage["rerank_reason"] = "current-source replacements were not reranked".into();
        if let Some(object) = coverage.as_object_mut() {
            object.remove("rerank_window");
        }
    }
    let mut replacements: HashMap<String, Vec<Hit>> = replacements
        .into_iter()
        .map(|(path, current)| (path.to_owned(), current))
        .collect();
    let unverified: HashSet<String> = unverified.into_iter().map(str::to_owned).collect();
    let mut replaced = HashSet::with_capacity(replacements.len());
    let mut current_hits = Vec::with_capacity(hits.len());
    for mut hit in std::mem::take(hits) {
        if let Some(current) = replacements.remove(&hit.path) {
            replaced.insert(hit.path);
            current_hits.extend(current);
        } else if !replaced.contains(&hit.path) {
            hit.differs |= unverified.contains(&hit.path);
            current_hits.push(hit);
        }
    }
    *hits = current_hits;
    Ok(())
}

fn missing(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    })
}

#[cfg(test)]
mod tests;
