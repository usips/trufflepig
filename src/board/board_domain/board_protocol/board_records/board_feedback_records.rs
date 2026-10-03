//! Feedback audit metadata and classified entry records.
use super::EntryRecord;
use crate::board::{
    board_ids::RepoKey,
    board_protocol::bounded_metadata,
    board_vocabulary::{FeedbackKind, FeedbackState},
};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecentCall {
    pub verb: String,
    pub args: Vec<String>,
    pub exit_code: Option<i32>,
    pub error_prefix: Option<String>,
    pub truncated: Option<bool>,
    pub coverage: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackMetadata {
    pub version: String,
    pub build_id: Option<String>,
    pub repo_key: Option<RepoKey>,
    pub cwd: String,
    pub steer_mode: Option<String>,
    pub recent_calls: Vec<RecentCall>,
}

impl FeedbackMetadata {
    pub fn validate(&self) -> Result<()> {
        bounded_metadata(&self.version, 256, "feedback version")?;
        bounded_metadata(&self.cwd, 4096, "feedback cwd")?;
        if let Some(build) = &self.build_id {
            bounded_metadata(build, 256, "feedback build id")?;
        }
        if let Some(steer) = &self.steer_mode {
            bounded_metadata(steer, 256, "feedback steering mode")?;
        }
        if self.recent_calls.len() > 5 {
            bail!("invalid_options: feedback accepts at most five recent calls");
        }
        if serde_json::to_vec(&self.recent_calls)?.len() > 2048 {
            bail!("invalid_options: recent calls exceed 2048 encoded bytes");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeedbackRecord {
    pub entry: EntryRecord,
    pub kind: FeedbackKind,
    pub state: FeedbackState,
    pub metadata: FeedbackMetadata,
}
