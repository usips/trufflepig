//! Plan revisions, tasks, claims, and review evidence.
use super::{EntryRecord, FeedbackRecord, LinkedCommit};
use crate::board::board_domain::board_collections::{ClaimCursor, EntryCursor, TaskCeiling};
use crate::board::{
    board_actor::{AgentVendor, BoardActor, BoardRecipient, HarnessLabel},
    board_ids::{EntryId, EventSeq, PlanId, PlanRevision, RepoKey, TaskId},
    board_vocabulary::{EntryText, PlanText, PlanTitle, ProposalState, TaskColumn},
};
use crate::identity::GitOid;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlanRecord {
    pub id: PlanId,
    pub title: PlanTitle,
    pub owner_user: String,
    pub steward: Option<HarnessLabel>,
    pub head_revision: u64,
    pub created_at: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionSource {
    Create,
    Accept,
    Direct,
}

impl RevisionSource {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "create" => Ok(Self::Create),
            "accept" => Ok(Self::Accept),
            "direct" => Ok(Self::Direct),
            _ => bail!("invalid_state: unknown revision source '{value}'"),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Accept => "accept",
            Self::Direct => "direct",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RevisionRecord {
    pub id: PlanRevision,
    pub body: PlanText,
    pub source: RevisionSource,
    pub entry: EntryId,
    pub actor: BoardActor,
    pub seq: EventSeq,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: TaskId,
    pub title: PlanTitle,
    pub column: TaskColumn,
    pub assignee: Option<BoardRecipient>,
    pub section: Option<String>,
    pub seq: EventSeq,
    pub done_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimEndReason {
    Released,
    TakenOver,
    Reassigned,
    Resumed,
}

impl ClaimEndReason {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "released" => Ok(Self::Released),
            "taken_over" => Ok(Self::TakenOver),
            "reassigned" => Ok(Self::Reassigned),
            "resumed" => Ok(Self::Resumed),
            _ => bail!("invalid_state: unknown claim end reason '{value}'"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Released => "released",
            Self::TakenOver => "taken_over",
            Self::Reassigned => "reassigned",
            Self::Resumed => "resumed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClaimRecord {
    pub task: TaskId,
    pub actor: BoardActor,
    pub vendor: AgentVendor,
    pub entry: EntryId,
    pub scope: EntryText,
    pub claimed_at: i64,
    pub last_active: i64,
    pub ended_at: Option<i64>,
    pub end_reason: Option<ClaimEndReason>,
    pub stale: bool,
    pub model: Option<String>,
    pub effort: Option<String>,
    #[serde(default)]
    pub delegated_by: Option<BoardActor>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProposalRecord {
    pub entry: EntryId,
    pub plan: PlanId,
    pub base_revision: u64,
    pub body: PlanText,
    pub state: ProposalState,
    pub stale_base: bool,
    pub decision_entry: Option<EntryId>,
    pub result_revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlanView {
    pub plan: PlanRecord,
    pub repo_keys: Vec<RepoKey>,
    pub revision: RevisionRecord,
    pub tasks: Vec<TaskRecord>,
    pub task_ceiling: TaskCeiling,
    pub claims: Vec<ClaimRecord>,
    pub entries: Vec<EntryRecord>,
    pub commits: Vec<LinkedCommit>,
    pub tasks_omitted: usize,
    pub claims_omitted: usize,
    pub entries_omitted: usize,
    pub commits_omitted: usize,
    pub tasks_next_after: Option<TaskId>,
    pub claims_next_after: Option<ClaimCursor>,
    pub entries_next_before: Option<EntryCursor>,
    pub through: EventSeq,
    pub can_edit: bool,
    pub server_now: i64,
    pub claim_ttl_secs: u64,
    pub sections_without_tasks: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RevisionDiff {
    pub before: RevisionRecord,
    pub after: RevisionRecord,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReviewEvidence {
    pub agent: Option<HarnessLabel>,
    pub window_end: i64,
    pub plan: PlanRecord,
    pub base: RevisionRecord,
    pub head: RevisionRecord,
    pub entries: Vec<EntryRecord>,
    pub tasks: Vec<TaskRecord>,
    pub claims: Vec<ClaimRecord>,
    pub commits: Vec<LinkedCommit>,
    pub manual_links: Vec<ManualCommitLink>,
    pub open_proposals: Vec<ProposalRecord>,
    pub open_questions: Vec<EntryRecord>,
    pub open_feedback: Vec<FeedbackRecord>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ManualCommitLink {
    pub repo_key: RepoKey,
    pub oid: GitOid,
    pub task: TaskId,
    pub entry: EntryId,
    pub seq: Option<EventSeq>,
    #[serde(rename = "linked_by")]
    pub actor: Option<BoardActor>,
}
