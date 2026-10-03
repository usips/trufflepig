//! Versioned replies and the records they contain.
use super::{
    AttentionReply, BOARD_API, ClaimPage, EntriesPage, EventPage, FeedbackPage, HistoryPage,
    OverviewReply, TaskPage,
};
use crate::board::{
    board_actor::BoardActor, board_ids::EventSeq, board_vocabulary::FeedbackImportKey,
};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

mod board_entry_records;
mod board_feedback_records;
mod board_plan_records;
mod board_repo_records;
mod board_search_records;

pub use board_entry_records::{
    BoardChange, EntryRecord, EntryState, EntryView, EventRecord, FeedbackVia, InboxReply,
    InboxWait,
};
pub use board_feedback_records::{FeedbackMetadata, FeedbackRecord, RecentCall};
pub use board_plan_records::{
    ClaimEndReason, ClaimRecord, PlanRecord, PlanView, ProposalRecord, ReviewEvidence,
    RevisionDiff, RevisionRecord, RevisionSource, TaskRecord,
};
pub use board_repo_records::{
    CommitCoauthor, CommitLinkResult, CommitPlanLink, LinkedCommit, RepoRegistration,
    RepoScanTarget,
};
pub use board_search_records::{BoardSearchHit, BoardSearchReply, BoardSearchSource};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardReply {
    pub api: u32,
    pub backend: String,
    pub result: BoardResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_seq: Option<EventSeq>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl BoardReply {
    pub fn new(backend: impl Into<String>, result: BoardResult) -> Self {
        Self {
            api: BOARD_API,
            backend: backend.into(),
            result,
            snapshot_seq: None,
            warnings: Vec::new(),
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.api != BOARD_API {
            bail!(
                "board_api_mismatch: expected {BOARD_API}, received {}",
                self.api
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", content = "data", rename_all = "snake_case")]
pub enum BoardResult {
    Session(SessionRecord),
    Inbox(InboxReply),
    Cursor(EventSeq),
    Plan(PlanView),
    Entry(EntryView),
    Search(BoardSearchReply),
    Revision(RevisionRecord),
    Diff(RevisionDiff),
    Change(BoardChange),
    Review(ReviewEvidence),
    Overview(OverviewReply),
    Attention(AttentionReply),
    Feed(EventPage),
    History(HistoryPage),
    Entries(EntriesPage),
    Tasks(TaskPage),
    Claims(ClaimPage),
    Feedback(FeedbackPage),
    Repositories(Vec<RepoScanTarget>),
    Registered(RepoRegistration),
    CommitsLinked(CommitLinkResult),
    Queued { import_key: FeedbackImportKey },
    ScanRecorded,
    RepoPathForgotten,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub actor: BoardActor,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub cursor: EventSeq,
    pub first_seen: i64,
    pub last_seen: i64,
}
