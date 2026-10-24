use crate::{
    daemon::deadline::{QueryDeadline, TIMED_OUT},
    identity::ContentRevision,
    results::ResultSet,
    source::{self, acquisition::stateless_hit_handle},
    store,
};
use anyhow::{Result, bail};
use std::{collections::HashMap, path::Path};

/// Attaches root-bound current-path cursors and discards hits without a verified revision.
pub(super) fn attach_current_handles(
    root: &Path,
    set: &mut ResultSet,
    deadline: &QueryDeadline,
) -> Result<usize> {
    let mut by_path: HashMap<String, Vec<usize>> = HashMap::with_capacity(set.hits.len());
    let mut missing_revision = 0usize;
    for (index, hit) in set.hits.iter().enumerate() {
        if hit.revision.is_none() {
            missing_revision += 1;
        } else {
            by_path.entry(hit.path.clone()).or_default().push(index);
        }
    }
    for (encoded_path, indices) in by_path {
        check_deadline(deadline)?;
        let relative = store::decode_path(&encoded_path)?;
        let bytes = source::read_contained(root, &relative, source::MAX_READ_BYTES)?;
        let revision = ContentRevision::of(&bytes).to_string();
        for index in indices {
            let hit = &mut set.hits[index];
            if hit.revision.as_deref() != Some(revision.as_str()) {
                bail!("stale_source: current file changed during stateless search; search again");
            }
            let (start, end) = if hit.kind == "file" && hit.start == 0 && hit.end == 0 {
                (0, bytes.len())
            } else {
                (hit.start, hit.end)
            };
            hit.handle = stateless_hit_handle(root, &relative, &bytes, start, end)?;
        }
    }
    if missing_revision > 0 {
        set.hits.retain(|hit| hit.revision.is_some());
    }
    Ok(missing_revision)
}

fn check_deadline(deadline: &QueryDeadline) -> Result<()> {
    if deadline.expired() {
        bail!("{TIMED_OUT}: stateless current-source handle verification expired");
    }
    Ok(())
}
