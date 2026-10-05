//! Entries, inbox events, and mutation addresses.
use super::{FeedbackRecord, LinkedCommit, ProposalRecord};
use crate::board::board_domain::board_collections::EntryCursor;
use crate::board::{
    board_actor::{BoardActor, BoardRecipient},
    board_ids::{BoardRef, EntryId, EventSeq, PlanId, PlanRevision, RepoKey, TaskId},
    board_vocabulary::{EntryKind, EntryText, FeedbackState, ProposalState},
};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "state", rename_all = "snake_case")]
pub enum EntryState {
    Proposal(ProposalState),
    Feedback(FeedbackState),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackVia {
    Outbox,
}
impl FeedbackVia {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "outbox" => Ok(Self::Outbox),
            _ => bail!("invalid_state: unknown feedback provenance '{value}'"),
        }
    }
    pub fn as_str(self) -> &'static str {
        "outbox"
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntryRecord {
    pub id: EntryId,
    pub via: Option<FeedbackVia>,
    pub plan: Option<PlanId>,
    pub kind: EntryKind,
    pub body: EntryText,
    pub to: Option<BoardRecipient>,
    pub supersedes: Option<EntryId>,
    pub actor: BoardActor,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub repo_key: Option<RepoKey>,
    pub state: Option<EntryState>,
    pub refs: Vec<BoardRef>,
    pub seq: EventSeq,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntryView {
    pub entry: EntryRecord,
    pub replies: Vec<EntryRecord>,
    pub replies_omitted: usize,
    pub backrefs: Vec<EntryRecord>,
    pub backrefs_omitted: usize,
    pub replies_next_after: Option<EntryCursor>,
    pub backrefs_next_after: Option<EntryCursor>,
    pub through: EventSeq,
    pub proposal: Option<ProposalRecord>,
    pub feedback: Option<FeedbackRecord>,
    pub linked_commit: Option<LinkedCommit>,
    pub can_decide: bool,
    pub can_supersede: bool,
    pub can_answer: bool,
    pub can_triage: bool,
    pub can_close: bool,
    pub plan_head_revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventRecord {
    pub via: Option<FeedbackVia>,
    pub seq: EventSeq,
    pub plan: Option<PlanId>,
    pub kind: EntryKind,
    pub subject: BoardRef,
    pub to: Option<BoardRecipient>,
    pub actor: BoardActor,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub summary: EntryText,
    pub created_at: i64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboxWait {
    #[default]
    None,
    Ready,
    Timeout,
    Busy,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InboxReply {
    pub actor: BoardActor,
    pub cursor: EventSeq,
    pub scanned_through: EventSeq,
    pub query_truncated: bool,
    pub events: Vec<EventRecord>,
    pub open: Vec<EntryRecord>,
    pub open_omitted: usize,
    /// True when the reminder count hit its cap, so the omitted total is a lower bound.
    #[serde(default)]
    pub open_omitted_lower_bound: bool,
    pub repo_key: Option<RepoKey>,
    pub all: bool,
    pub latest: EventSeq,
    pub advancing: bool,
    pub wait: InboxWait,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoardChange {
    pub entry: EntryId,
    pub seq: EventSeq,
    pub plan: Option<PlanId>,
    pub revision: Option<PlanRevision>,
    pub task: Option<TaskId>,
    pub deduplicated: bool,
}
