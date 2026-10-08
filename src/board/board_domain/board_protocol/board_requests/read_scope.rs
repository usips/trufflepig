//! Concrete repository scopes resolved by the serving host before board reads.
use crate::board::board_ids::RepoKey;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ReadScope {
    All,
    /// Includes unlinked plans and retains addressed and own-feedback exceptions.
    Repo(RepoKey),
    /// Includes plans linked to at least one key, excluding unlinked plans.
    Keys(BTreeSet<RepoKey>),
    /// Includes real plans without a repository association.
    Unscoped,
}

impl ReadScope {
    pub fn is_all(&self) -> bool {
        matches!(self, Self::All)
    }
}
