use super::coverage::{CurrentCoverage, LIMIT_DEADLINE};
use crate::{
    results::ResultSet,
    source::{self, acquisition::stateless_hit_handle},
};
use anyhow::{Result, bail};
use std::{collections::HashMap, path::Path};
use tp_core::{identity::ContentRevision, query_deadline::QueryDeadline};

/// Attaches root-bound current-path cursors in rank order. A path-lane `file`
/// hit on a file the scan did not extract gets its revision from these bytes;
/// unreadable or binary ones are dropped as excluded. When the deadline
/// expires, the unverified remainder is dropped and the page stays partial.
pub(super) fn attach_current_handles(
    root: &Path,
    set: &mut ResultSet,
    deadline: &QueryDeadline,
    current: &mut CurrentCoverage,
) -> Result<()> {
    let mut by_path: HashMap<String, Vec<usize>> = HashMap::with_capacity(set.hits.len());
    let mut order = Vec::with_capacity(set.hits.len());
    for (index, hit) in set.hits.iter().enumerate() {
        by_path
            .entry(hit.path.clone())
            .or_insert_with(|| {
                order.push(hit.path.clone());
                Vec::new()
            })
            .push(index);
    }
    let mut verified = vec![false; set.hits.len()];
    let mut unverified_files = 0usize;
    let mut dropped = 0usize;
    for (position, encoded_path) in order.iter().enumerate() {
        if deadline.expired() {
            unverified_files = order.len() - position;
            break;
        }
        let indices = &by_path[encoded_path];
        let extracted = indices
            .iter()
            .any(|&index| set.hits[index].revision.is_some());
        let relative = tp_core::path_codec::decode_path(encoded_path)?;
        let bytes = match source::read_contained(root, &relative, source::MAX_READ_BYTES) {
            Ok(bytes) => bytes,
            Err(error) if extracted => return Err(error),
            Err(_) => {
                dropped += indices.len();
                continue;
            }
        };
        if !extracted && bytes.contains(&0) {
            dropped += indices.len();
            continue;
        }
        let revision = ContentRevision::of(&bytes).to_string();
        for &index in indices {
            let hit = &mut set.hits[index];
            match hit.revision.as_deref() {
                Some(expected) if expected != revision => bail!(coded!(
                    StaleSource,
                    "current file changed during stateless search; search again"
                )),
                Some(_) => {}
                None if hit.kind == "file" => hit.revision = Some(revision.clone()),
                None => {
                    dropped += 1;
                    continue;
                }
            }
            let (start, end) = if hit.kind == "file" && hit.start == 0 && hit.end == 0 {
                (0, bytes.len())
            } else {
                (hit.start, hit.end)
            };
            hit.handle = stateless_hit_handle(root, &relative, &bytes, start, end)?;
            verified[index] = true;
        }
    }
    if dropped > 0 {
        // Unreadable or binary path matches cannot carry a handle; the page omits them.
        current.excluded_files = current.excluded_files.saturating_add(dropped);
        set.truncated = true;
    }
    if unverified_files > 0 {
        current.stop(LIMIT_DEADLINE, unverified_files, None);
    }
    let mut index = 0;
    set.hits.retain(|_| {
        index += 1;
        verified[index - 1]
    });
    Ok(())
}
