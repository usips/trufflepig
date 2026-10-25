//! Bounded task detail records.
use super::{ClaimRecord, EntryRecord, LinkedCommit, TaskRecord};
use crate::board::board_ids::{EntryId, EventSeq};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskView {
    pub task: TaskRecord,
    pub current_claim: Option<ClaimRecord>,
    pub claim_history: Vec<ClaimRecord>,
    pub entries: Vec<EntryRecord>,
    pub commits: Vec<LinkedCommit>,
    pub open_questions: usize,
    pub oldest_open_question: Option<EntryId>,
    pub oldest_open_question_at: Option<i64>,
    pub latest_commit_at: Option<i64>,
    pub through: EventSeq,
    pub server_now: i64,
    pub claim_ttl_secs: u64,
    pub claims_omitted: usize,
    pub entries_omitted: usize,
    pub commits_omitted: usize,
}
