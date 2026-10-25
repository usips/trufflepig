//! Versioned, typed board operations and evidence returned by the backend.
mod board_authority;
mod board_errors;
mod board_identity;
mod board_progress;
mod board_records;
mod board_requests;
mod board_verdict;
#[cfg(test)]
mod tests;

pub use super::board_collections::*;
pub use crate::board::board_vocabulary::{AuthorityRole, WorkerEndReason, WorkerRole};
pub use board_authority::AuthoritySource;
pub use board_errors::{BoardError, BoardErrorCode};
pub use board_identity::{WorkerAddress, WorkerHandle};
pub use board_progress::{
    GateStatus, PROGRESS_ETA_LIMIT_SECS, ProgressEvidence, ProgressFact, ProgressGateFact,
    ProgressGateInput, ProgressHead, ProgressInput, ProgressStep, ProgressTests,
};
pub use board_records::{
    BoardChange, BoardReceipt, BoardReceiptState, BoardReply, BoardResult, BoardSearchHit,
    BoardSearchReply, BoardSearchSource, COAUTHOR_LIMIT, ClaimEndReason, ClaimRecord,
    CommitClaimBasis, CommitClaimLink, CommitCoauthor, CommitLinkResult, CommitPlanLink,
    EntryRecord, EntryState, EntryView, EventRecord, FeedbackMetadata, FeedbackRecord, FeedbackVia,
    GrantRecord, GrantSource, InboxReply, InboxWait, LINK_LIMIT, LinkedCommit, ManualCommitLink,
    PendingNotice, PendingNoticeKind, PendingNoticeReason, PendingReply, PlanLifecycleState,
    PlanRecord, PlanView, ProjectMemberRecord, ProjectRecord, ProposalRecord, RecentCall,
    RepoRegistration, RepoScanTarget, ReviewEvidence, RevisionDiff, RevisionRecord, RevisionSource,
    SessionRecord, TaskRecord, TaskView, WORKER_FILE_LIMIT, WORKER_FILE_PATH_LIMIT,
    WORKER_REF_LIMIT, WorkerRecord,
};
pub use board_requests::{
    AgentClaims, BoardOp, BoardRequest, BoardRequestKey, ClaimDelegate, ClaimResume, ReadScope,
};
pub use board_verdict::ReviewVerdict;

use anyhow::{Result, bail};

pub const BOARD_API: u32 = 13;
pub const BOARD_WRITE_RECEIPT_RETENTION_SECS: i64 = 600;

fn validate_claim(value: &str, field: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        bail!(coded!(
            InvalidOptions,
            "{field} must contain 1..256 bytes without control characters"
        ));
    }
    Ok(())
}

fn bounded_metadata(value: &str, limit: usize, field: &str) -> Result<()> {
    if value.len() > limit || value.contains('\0') {
        bail!(coded!(
            InvalidOptions,
            "{field} exceeds {limit} bytes or contains NUL"
        ));
    }
    Ok(())
}
