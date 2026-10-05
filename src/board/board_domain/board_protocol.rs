//! Versioned, typed board operations and evidence returned by the backend.
mod board_errors;
mod board_records;
mod board_requests;
#[cfg(test)]
mod tests;

pub use super::board_collections::*;
pub use board_errors::{BoardError, BoardErrorCode, leading_error_code};
pub use board_records::{
    BoardChange, BoardReply, BoardResult, BoardSearchHit, BoardSearchReply, BoardSearchSource,
    COAUTHOR_LIMIT, ClaimEndReason, ClaimRecord, CommitCoauthor, CommitLinkResult, CommitPlanLink,
    EntryRecord, EntryState, EntryView, EventRecord, FeedbackMetadata, FeedbackRecord, FeedbackVia,
    InboxReply, InboxWait, LINK_LIMIT, LinkedCommit, PlanRecord, PlanView, ProposalRecord,
    RecentCall, RepoRegistration, RepoScanTarget, ReviewEvidence, RevisionDiff, RevisionRecord,
    RevisionSource, SessionRecord, TaskRecord,
};
pub use board_requests::{AgentClaims, BoardOp, BoardRequest, ClaimDelegate, ClaimResume};

use anyhow::{Result, bail};

pub const BOARD_API: u32 = 4;

fn validate_claim(value: &str, field: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        bail!("invalid_options: {field} must contain 1..256 bytes without control characters");
    }
    Ok(())
}

fn bounded_metadata(value: &str, limit: usize, field: &str) -> Result<()> {
    if value.len() > limit || value.contains('\0') {
        bail!("invalid_options: {field} exceeds {limit} bytes or contains NUL");
    }
    Ok(())
}
