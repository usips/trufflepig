//! Versioned replies and the records they contain.
use super::BoardRequestKey;
use super::{
    AttentionReply, BOARD_API, ClaimPage, DoneTasksPage, EntriesPage, EventPage, FeedbackPage,
    HistoryPage, OverviewReply, TaskPage,
};
use crate::{board_actor::BoardActor, board_ids::EventSeq, board_vocabulary::FeedbackImportKey};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

mod board_entry_records;
mod board_feedback_records;
mod board_grant_records;
mod board_pending_records;
mod board_plan_records;
mod board_project_records;
mod board_repo_records;
mod board_search_records;
mod board_task_records;
mod board_worker_records;

pub use board_entry_records::{
    BoardChange, EntryRecord, EntryState, EntryView, EventRecord, FeedbackVia, InboxReply,
    InboxWait,
};
pub use board_feedback_records::{FeedbackMetadata, FeedbackRecord, RecentCall};
pub use board_grant_records::{GrantRecord, GrantSource};
pub use board_pending_records::{
    PendingNotice, PendingNoticeKind, PendingNoticeReason, PendingReply,
};
pub use board_plan_records::{
    ClaimEndReason, ClaimRecord, ManualCommitLink, PlanLifecycleState, PlanRecord, PlanView,
    ProposalRecord, ReviewEvidence, RevisionDiff, RevisionRecord, RevisionSource, TaskRecord,
};
pub use board_project_records::{ProjectMemberRecord, ProjectRecord};
pub use board_repo_records::{
    COAUTHOR_LIMIT, CommitClaimBasis, CommitClaimLink, CommitCoauthor, CommitLinkResult,
    CommitPlanLink, LINK_LIMIT, LinkedCommit, RepoRegistration, RepoScanTarget, WORKER_REF_LIMIT,
};
pub use board_search_records::{BoardSearchHit, BoardSearchReply, BoardSearchSource};
pub use board_task_records::TaskView;
pub use board_worker_records::{WORKER_FILE_LIMIT, WORKER_FILE_PATH_LIMIT, WorkerRecord};

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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardReceiptState {
    Committed,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardReceipt {
    pub key: BoardRequestKey,
    pub state: BoardReceiptState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<BoardChange>,
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
            bail!(coded!(
                BoardApiMismatch,
                "expected {BOARD_API}, received {}",
                self.api
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", content = "data", rename_all = "snake_case")]
pub enum BoardResult {
    Session(SessionRecord),
    Bye(ByeReply),
    Inbox(InboxReply),
    Cursor(EventSeq),
    Plan(PlanView),
    Entry(EntryView),
    Search(BoardSearchReply),
    Revision(RevisionRecord),
    Diff(RevisionDiff),
    Change(BoardChange),
    Receipt(BoardReceipt),
    Review(ReviewEvidence),
    Overview(OverviewReply),
    Attention(AttentionReply),
    Feed(EventPage),
    History(HistoryPage),
    Entries(EntriesPage),
    Tasks(TaskPage),
    TaskView(TaskView),
    DoneTasks(DoneTasksPage),
    Claims(ClaimPage),
    Pending(PendingReply),
    Feedback(FeedbackPage),
    Repositories(Vec<RepoScanTarget>),
    Projects(Vec<ProjectRecord>),
    Registered(RepoRegistration),
    CommitsLinked(CommitLinkResult),
    Queued { import_key: FeedbackImportKey },
    ScanRecorded,
    RepoPathForgotten,
}

/// What `bye` did to the caller's agent session.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByeOutcome {
    /// This call ended a live session and recorded a bye entry and event.
    Ended,
    /// The session had already ended; nothing was written.
    AlreadyEnded,
    /// The identity never said hello or wrote; nothing was written.
    NoSession,
}

/// A bye result; `entry`/`seq` name this call's bye for `Ended`, the original
/// bye for `AlreadyEnded` when one exists, and are absent for `NoSession`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ByeReply {
    pub outcome: ByeOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<crate::board_ids::EntryId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<EventSeq>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub actor: BoardActor,
    pub model: Option<String>,
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<super::WorkerAddress>,
    pub cursor: EventSeq,
    pub first_seen: i64,
    pub last_seen: i64,
}
