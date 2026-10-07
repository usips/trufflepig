//! Exact per-commit warning identities and current board-wide link suppression.
use crate::board::{board_backend::BoardBackend, board_ids::RepoKey};
use crate::identity::GitOid;
use anyhow::Result;

pub(super) fn commit_warning_oid(warning: &str) -> Option<GitOid> {
    let (oid, _) = warning.strip_prefix("board_scan: ")?.split_once(": ")?;
    GitOid::parse(oid).ok()
}

pub(crate) fn suppress_linked_warnings(
    backend: &dyn BoardBackend,
    repo_key: &RepoKey,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let mut oids = Vec::with_capacity(warnings.len());
    oids.extend(
        warnings
            .iter()
            .filter_map(|warning| commit_warning_oid(warning)),
    );
    oids.sort_unstable();
    oids.dedup();
    if oids.is_empty() {
        return Ok(());
    }
    let linked = backend.linked_commit_oids(repo_key, &oids)?;
    warnings.retain(|warning| commit_warning_oid(warning).is_none_or(|oid| !linked.contains(&oid)));
    Ok(())
}
