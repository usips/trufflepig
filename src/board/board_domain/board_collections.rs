//! Bounded collection reads and frozen sequence cursors for the board API.
//! Reuse a cursor with its returned `through` value to preserve source membership.
//! Mutable fields reflect the current transaction snapshot, rather than historical state.
//! Task pages freeze membership with an ordinal ceiling, independent of task moves.
use super::board_actor::BoardActor;
use super::board_ids::{
    EntryId, EventSeq, MAX_BOARD_NUMBER, PlanId, PlanRevision, RepoKey, TaskId,
};
use super::board_protocol::{
    ClaimRecord, EntryRecord, EventRecord, FeedbackRecord, PlanRecord, ReadScope, RevisionSource,
    TaskRecord,
};
use super::board_vocabulary::EntryText;
use super::board_vocabulary::TaskColumn;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OverviewReply {
    pub plans: Vec<PlanOverview>,
    pub omitted: usize,
    pub scope: ReadScope,
    pub server_now: i64,
    pub claim_ttl_secs: u64,
    pub after: Option<PlanId>,
    pub through: EventSeq,
    pub next_after: Option<PlanId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlanOverview {
    pub plan: PlanRecord,
    pub repo_keys: Vec<RepoKey>,
    pub tasks: Vec<TaskRecord>,
    pub task_ceiling: TaskCeiling,
    pub done_count: usize,
    pub recent_done: Vec<TaskRecord>,
    pub claims: Vec<ClaimRecord>,
    pub open_questions: usize,
    pub open_proposals: usize,
    pub open_feedback: usize,
    pub tasks_omitted: usize,
    pub claims_omitted: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttentionReply {
    pub actor: BoardActor,
    pub entries: Vec<EntryRecord>,
    pub entries_omitted: usize,
    pub stale_claims: Vec<ClaimView>,
    pub claims_omitted: usize,
    pub rebase_needed: Vec<EntryId>,
    pub scope: ReadScope,
    pub server_now: i64,
    pub claim_ttl_secs: u64,
    pub after: Option<EntryCursor>,
    pub through: EventSeq,
    pub next_after: Option<EntryCursor>,
    pub claims_next_after: Option<ClaimCursor>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventPage {
    pub plan: Option<PlanId>,
    pub scope: ReadScope,
    pub events: Vec<EventRecord>,
    pub after: EventSeq,
    pub through: EventSeq,
    pub next_after: Option<EventSeq>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RevisionHistoryRecord {
    pub id: PlanRevision,
    pub source: RevisionSource,
    pub actor: BoardActor,
    pub summary: EntryText,
    pub seq: EventSeq,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryPage {
    pub plan: PlanId,
    pub revisions: Vec<RevisionHistoryRecord>,
    pub after: EventSeq,
    pub through: EventSeq,
    pub next_after: Option<EventSeq>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryCursor {
    pub seq: EventSeq,
    pub entry: EntryId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntriesPage {
    pub plan: Option<PlanId>,
    pub references: Option<EntryId>,
    pub entries: Vec<EntryRecord>,
    pub after: Option<EntryCursor>,
    pub through: EventSeq,
    pub next_after: Option<EntryCursor>,
    pub next_before: Option<EntryCursor>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeedbackPage {
    pub open_only: bool,
    pub feedback: Vec<FeedbackRecord>,
    pub after: Option<EntryCursor>,
    pub through: EventSeq,
    pub next_after: Option<EntryCursor>,
    pub omitted: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskPage {
    pub plan: PlanId,
    pub column: Option<TaskColumn>,
    pub order: TaskOrder,
    pub tasks: Vec<TaskRecord>,
    pub after: Option<TaskId>,
    pub before: Option<TaskCursor>,
    pub ceiling: TaskCeiling,
    pub through: EventSeq,
    pub next_after: Option<TaskId>,
    pub next_before: Option<TaskCursor>,
    pub omitted: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOrder {
    #[default]
    Ordinal,
    RecentFirst,
}

/// Completion sequence and numeric task identity form a descending paging key.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCursor {
    pub seq: EventSeq,
    pub id: TaskId,
}

impl TaskCursor {
    pub fn validate(self) -> anyhow::Result<()> {
        self.id.validate()?;
        anyhow::ensure!(
            (1..=MAX_BOARD_NUMBER).contains(&self.seq.get()),
            "invalid_reference: task cursor must contain a positive SQLite sequence"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DoneTasksPage {
    pub scope: super::board_protocol::ReadScope,
    pub tasks: Vec<TaskRecord>,
    pub before: Option<TaskCursor>,
    pub next_before: Option<TaskCursor>,
    pub omitted: usize,
    pub server_now: i64,
}

/// The captured maximum task ordinal freezes membership, including an empty plan at zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCeiling {
    pub plan: PlanId,
    pub ordinal: u64,
}

impl TaskCeiling {
    pub fn validate(self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.ordinal <= MAX_BOARD_NUMBER,
            "invalid_reference: task ceiling exceeds the SQLite ordinal range"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClaimPage {
    pub plan: Option<PlanId>,
    pub own_stale: bool,
    pub scope: ReadScope,
    pub claims: Vec<ClaimView>,
    pub after: Option<ClaimCursor>,
    pub through: EventSeq,
    pub next_after: Option<ClaimCursor>,
    pub omitted: usize,
    pub server_now: i64,
    pub claim_ttl_secs: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Claims can share entry IDs; the positive SQLite claim ID breaks pagination ties.
pub struct ClaimCursor {
    pub entry: EntryId,
    pub claim: u64,
}

impl ClaimCursor {
    pub fn validate(self) -> anyhow::Result<()> {
        anyhow::ensure!(
            (1..=MAX_BOARD_NUMBER).contains(&self.claim),
            "invalid_reference: claim cursor must contain a positive SQLite id"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClaimView {
    pub claim: ClaimRecord,
    pub cursor: ClaimCursor,
}
