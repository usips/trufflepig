//! Serving-host project availability and board membership counts.
use crate::board::board_ids::RepoKey;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub id: String,
    pub name: String,
    pub repo_keys: BTreeSet<RepoKey>,
    pub unavailable: Vec<ProjectMemberRecord>,
    pub plan_count: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectMemberRecord {
    pub name: String,
    /// Percent-encoded Unix path bytes, using `store::encode_path`.
    pub root: String,
    pub available: bool,
}
