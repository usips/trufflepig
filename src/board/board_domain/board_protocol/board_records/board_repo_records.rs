//! Portable repository registration and linked commit evidence.
use crate::{
    board::{
        board_actor::HarnessLabel,
        board_ids::{PlanId, RepoKey, TaskId},
    },
    identity::GitOid,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Maximum co-authors per linked commit; the scanner and wire agree.
pub const COAUTHOR_LIMIT: usize = 64;
/// Maximum plan links per linked commit; the scanner and wire agree.
pub const LINK_LIMIT: usize = 256;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RepoRegistration {
    pub repo_key: RepoKey,
    pub origin_label: Option<String>,
    pub host: String,
    pub common_dir: PathBuf,
    pub plan_id: Option<PlanId>,
    #[serde(default)]
    pub root_commits: Vec<GitOid>,
    #[serde(default)]
    pub registration_error: Option<String>,
    #[serde(default)]
    pub origin_override: Option<RepoKey>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RepoScanTarget {
    pub registration: RepoRegistration,
    pub oldest_plan_at: i64,
    pub plans: Vec<PlanId>,
    pub scan_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommitCoauthor {
    pub harness: HarnessLabel,
    pub model: String,
    pub email: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommitPlanLink {
    pub plan_id: PlanId,
    pub task_ordinal: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinkedCommit {
    pub repo_key: RepoKey,
    pub oid: GitOid,
    pub subject: String,
    pub committed_at: i64,
    pub author: String,
    pub coauthors: Vec<CommitCoauthor>,
    pub files: u64,
    pub insertions: u64,
    pub deletions: u64,
    pub plans: Vec<CommitPlanLink>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CommitLinkResult {
    pub inserted: u64,
    pub unknown_plans: Vec<PlanId>,
    pub unknown_tasks: Vec<TaskId>,
}
