//! Tagged persisted identities share one result cache and expiry policy.
use super::Hit;
use crate::identity::{ByteSpan, ContentRevision, GitOid};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoricalSource {
    pub repository: String,
    pub commit: GitOid,
    pub blob: GitOid,
    pub revision: ContentRevision,
    pub path: String,
    pub span: ByteSpan,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommitEntry {
    #[serde(default)]
    pub handle: String,
    pub repository: String,
    pub commit: GitOid,
    pub parent: Option<GitOid>,
    pub summary: String,
    pub committer_time: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChangeEntry {
    #[serde(default)]
    pub handle: String,
    pub name: String,
    pub status: String,
    pub before: Option<HistoricalSource>,
    pub after: Option<HistoricalSource>,
    pub correspondence: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "entry", rename_all = "snake_case")]
pub enum ResultEntry {
    LiveSource(Hit),
    Commit(CommitEntry),
    Change(ChangeEntry),
}
impl ResultEntry {
    pub fn handle_mut(&mut self) -> &mut String {
        match self {
            Self::LiveSource(hit) => &mut hit.handle,
            Self::Commit(commit) => &mut commit.handle,
            Self::Change(change) => &mut change.handle,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StoredResultSet {
    pub generation: i64,
    pub coverage: serde_json::Value,
    pub truncated: bool,
    pub hits: Vec<ResultEntry>,
}
