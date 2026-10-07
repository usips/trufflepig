//! Current task-linked commit identities independent of review windows and plans.
use super::super::{BoardError, sql_error};
use crate::board::board_ids::RepoKey;
use crate::identity::GitOid;
use rusqlite::{Connection, params};
use std::collections::BTreeSet;

#[cfg(test)]
mod tests;

pub(in crate::board::local_board) fn linked_commit_oids(
    conn: &Connection,
    repo_key: &RepoKey,
    oids: &[GitOid],
) -> Result<BTreeSet<GitOid>, BoardError> {
    if oids.is_empty() {
        return Ok(BTreeSet::new());
    }
    let candidates =
        serde_json::to_string(oids).map_err(|error| BoardError::from(anyhow::Error::new(error)))?;
    let mut statement = conn
        .prepare(concat!(
            "SELECT oid FROM commit_tasks WHERE repo_key=?1 ",
            "AND oid IN (SELECT value FROM json_each(?2))"
        ))
        .map_err(sql_error)?;
    let mut rows = statement
        .query(params![repo_key.as_str(), candidates])
        .map_err(sql_error)?;
    let mut linked = BTreeSet::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        let value: String = row.get(0).map_err(sql_error)?;
        linked.insert(GitOid::parse(&value).map_err(BoardError::from)?);
    }
    Ok(linked)
}
