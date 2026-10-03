//! Versioned, typed board operations and evidence returned by the backend.
use super::board_actor::{BoardActor, BoardRecipient, HarnessLabel, validate_actor_component};
use super::board_ids::{BoardRef, EntryId, EventSeq, PlanId, PlanRevision, RepoKey, TaskId};
use super::board_vocabulary::{
    EntryKind, EntryText, FeedbackImportKey, FeedbackKind, FeedbackState, PlanText, PlanTitle, ProposalState,
    TaskColumn,
};
use crate::identity::GitOid;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const BOARD_API: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardErrorCode {
    StaleRevision,
    ClaimConflict,
    BoardUnavailable,
    InvalidReference,
    InvalidKind,
    InvalidBody,
    InvalidActor,
    InvalidState,
    InvalidOptions,
    Usage,
    BoardApiMismatch,
    BoardRemoteUnsupported,
    DatabaseLocked,
    DaemonBusy,
}

impl BoardErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StaleRevision => "stale_revision",
            Self::ClaimConflict => "claim_conflict",
            Self::BoardUnavailable => "board_unavailable",
            Self::InvalidReference => "invalid_reference",
            Self::InvalidKind => "invalid_kind",
            Self::InvalidBody => "invalid_body",
            Self::InvalidActor => "invalid_actor",
            Self::InvalidState => "invalid_state",
            Self::InvalidOptions => "invalid_options",
            Self::Usage => "usage",
            Self::BoardApiMismatch => "board_api_mismatch",
            Self::BoardRemoteUnsupported => "board_remote_unsupported",
            Self::DatabaseLocked => "database is locked",
            Self::DaemonBusy => "daemon_busy",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BoardError {
    pub code: BoardErrorCode,
    pub message: String,
}

impl BoardError {
    pub fn new(code: BoardErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for BoardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}
impl std::error::Error for BoardError {}
impl From<anyhow::Error> for BoardError {
    fn from(error: anyhow::Error) -> Self {
        if let Some(typed) = error.downcast_ref::<BoardError>() {
            return typed.clone();
        }
        let message = format!("{error:#}");
        let codes = [
            BoardErrorCode::StaleRevision,
            BoardErrorCode::ClaimConflict,
            BoardErrorCode::BoardUnavailable,
            BoardErrorCode::InvalidReference,
            BoardErrorCode::InvalidKind,
            BoardErrorCode::InvalidBody,
            BoardErrorCode::InvalidActor,
            BoardErrorCode::InvalidState,
            BoardErrorCode::InvalidOptions,
            BoardErrorCode::Usage,
            BoardErrorCode::BoardApiMismatch,
            BoardErrorCode::BoardRemoteUnsupported,
            BoardErrorCode::DatabaseLocked,
            BoardErrorCode::DaemonBusy,
        ];
        for cause in error.chain() {
            let cause = cause.to_string();
            for code in codes {
                if let Some(detail) = cause
                    .strip_prefix(code.as_str())
                    .and_then(|tail| tail.strip_prefix(':'))
                {
                    return Self::new(code, detail.trim_start());
                }
            }
        }
        if message.contains("database is locked") {
            return Self::new(BoardErrorCode::DatabaseLocked, message);
        }
        Self::new(BoardErrorCode::BoardUnavailable, message)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardRequest {
    pub api: u32,
    pub actor: BoardActor,
    pub op: BoardOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claims: Option<AgentClaims>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentClaims {
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl BoardRequest {
    pub fn new(actor: BoardActor, op: BoardOp) -> Self {
        Self {
            api: BOARD_API,
            actor,
            op,
            claims: None,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.api != BOARD_API {
            bail!(
                "board_api_mismatch: expected {BOARD_API}, received {}",
                self.api
            );
        }
        self.actor.validate()?;
        if let Some(claims) = &self.claims {
            if let Some(model) = &claims.model {
                validate_claim(model, "model")?;
            }
            if let Some(effort) = &claims.effort {
                validate_claim(effort, "effort")?;
            }
        }
        self.op.validate()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum BoardOp {
    Hello {
        model: String,
        effort: Option<String>,
    },
    Inbox {
        after: Option<EventSeq>,
        limit: usize,
    },
    AcknowledgeInbox {
        rendered_through: EventSeq,
    },
    Show {
        target: Option<BoardRef>,
    },
    New {
        title: PlanTitle,
        body: PlanText,
        steward: Option<HarnessLabel>,
    },
    Post {
        target: BoardRef,
        kind: EntryKind,
        body: EntryText,
        to: Option<BoardRecipient>,
        supersedes: Option<EntryId>,
    },
    TaskCreate {
        plan: PlanId,
        title: PlanTitle,
        to: Option<BoardRecipient>,
        section: Option<String>,
    },
    TaskMove {
        task: TaskId,
        column: TaskColumn,
        to: Option<BoardRecipient>,
    },
    ClaimTask {
        task: TaskId,
        scope: EntryText,
    },
    CarveClaim {
        plan: PlanId,
        title: PlanTitle,
        scope: EntryText,
        section: Option<String>,
    },
    Propose {
        base: PlanRevision,
        body: PlanText,
        summary: EntryText,
    },
    Edit {
        base: PlanRevision,
        body: PlanText,
        summary: EntryText,
    },
    Accept {
        proposal: EntryId,
        note: Option<EntryText>,
    },
    Reject {
        proposal: EntryId,
        reason: EntryText,
    },
    Review {
        base: PlanRevision,
        agent: Option<HarnessLabel>,
    },
    Feedback {
        kind: FeedbackKind,
        summary: EntryText,
        body: Option<EntryText>,
        plan: Option<PlanId>,
        metadata: FeedbackMetadata,
        import_key: Option<FeedbackImportKey>,
    },
    FeedbackList {
        open_only: bool,
    },
    FeedbackClose {
        entry: EntryId,
        state: FeedbackState,
        note: Option<EntryText>,
    },
    RegisterRepo {
        registration: RepoRegistration,
    },
    LinkCommits {
        commits: Vec<LinkedCommit>,
    },
    Repositories {
        plan: Option<PlanId>,
    },
    RecordScan {
        repo_key: RepoKey,
        host: String,
        common_dir: PathBuf,
        error: Option<String>,
    },
}

impl BoardOp {
    pub fn is_read(&self) -> bool {
        matches!(
            self,
            Self::Inbox { after: Some(_), .. }
                | Self::Show { .. }
                | Self::Review { .. }
                | Self::FeedbackList { .. }
                | Self::Repositories { .. }
        )
    }

    pub fn plan_id(&self) -> Option<PlanId> {
        match self {
            Self::Post { target, .. }
            | Self::Show {
                target: Some(target),
            } => target.plan_id(),
            Self::TaskCreate { plan, .. } | Self::CarveClaim { plan, .. } => Some(*plan),
            Self::TaskMove { task, .. } | Self::ClaimTask { task, .. } => Some(task.plan),
            Self::Propose { base, .. } | Self::Edit { base, .. } | Self::Review { base, .. } => {
                Some(base.plan)
            }
            Self::Feedback { plan, .. } | Self::Repositories { plan } => *plan,
            Self::RegisterRepo { registration } => registration.plan_id,
            _ => None,
        }
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Post { target, .. }
            | Self::Show {
                target: Some(target),
            } => target.validate()?,
            Self::TaskMove { task, .. } | Self::ClaimTask { task, .. } => task.validate()?,
            Self::Propose { base, .. } | Self::Edit { base, .. } | Self::Review { base, .. } => {
                base.validate()?
            }
            _ => {}
        }
        match self {
            Self::Hello { model, effort } => {
                validate_claim(model, "model")?;
                if let Some(effort) = effort {
                    validate_claim(effort, "effort")?;
                }
            }
            Self::Inbox { limit, .. } if *limit == 0 || *limit > 2000 => {
                bail!("invalid_options: inbox limit must be 1..2000")
            }
            Self::Post { target, kind, .. } => {
                if !matches!(target, BoardRef::Plan(_) | BoardRef::Task(_)) {
                    bail!("invalid_reference: post requires a plan or task");
                }
                if !kind.is_post_kind() {
                    bail!("invalid_kind: this entry kind cannot be posted");
                }
            }
            Self::Show {
                target: Some(BoardRef::Entry(_) | BoardRef::Commit(_)),
            } => bail!("invalid_reference: show requires a plan, revision, or revision span"),
            Self::TaskCreate { section, .. } | Self::CarveClaim { section, .. } => {
                if let Some(section) = section {
                    validate_claim(section, "section")?;
                }
            }
            Self::FeedbackClose { state, .. } if !state.is_closed() => {
                bail!("invalid_state: close requires fixed, wontfix, or duplicate")
            }
            Self::Feedback {
                summary,
                body,
                metadata,
                ..
            } => {
                let length = summary.as_str().len()
                    + body.as_ref().map_or(0, |text| text.as_str().len() + 2);
                if length > super::board_vocabulary::ENTRY_TEXT_LIMIT {
                    bail!("invalid_body: feedback text exceeds 4096 bytes");
                }
                metadata.validate()?;
            }
            Self::LinkCommits { commits } => {
                if commits.len() > 2000 {
                    bail!("invalid_options: commit batch exceeds 2000 records");
                }
                for commit in commits {
                    bounded_metadata(&commit.subject, 1024, "commit subject")?;
                    bounded_metadata(&commit.author, 1024, "commit author")?;
                    if commit.coauthors.len() > 64
                        || commit.plans.len() > 256
                        || commit.file_stats.len() > 2000
                    {
                        bail!("invalid_options: commit metadata exceeds collection limits");
                    }
                    for number in [commit.files, commit.insertions, commit.deletions] {
                        if number > super::board_ids::MAX_BOARD_NUMBER {
                            bail!("invalid_options: commit statistics exceed SQLite integer range");
                        }
                    }
                    for author in &commit.coauthors {
                        bounded_metadata(&author.model, 256, "co-author model")?;
                        bounded_metadata(&author.email, 256, "co-author email")?;
                    }
                    for file in &commit.file_stats {
                        bounded_metadata(&file.path, 4096, "commit path")?;
                        for number in [file.insertions, file.deletions].into_iter().flatten() {
                            if number > super::board_ids::MAX_BOARD_NUMBER {
                                bail!(
                                    "invalid_options: file statistics exceed SQLite integer range"
                                );
                            }
                        }
                    }
                    for link in &commit.plans {
                        if let Some(ordinal) = link.task_ordinal {
                            TaskId::new(link.plan_id, ordinal)?;
                        }
                    }
                }
            }
            Self::RegisterRepo { registration } => {
                validate_actor_component(&registration.host, "repository host")?;
                if !registration.common_dir.is_absolute() {
                    bail!("invalid_options: repository common_dir must be absolute");
                }
                if let Some(origin) = &registration.origin_label {
                    bounded_metadata(origin, 4096, "origin label")?;
                }
            }
            Self::RecordScan {
                host,
                common_dir,
                error,
                ..
            } => {
                validate_actor_component(host, "repository host")?;
                if !common_dir.is_absolute() {
                    bail!("invalid_options: repository common_dir must be absolute");
                }
                if let Some(error) = error {
                    bounded_metadata(error, 4096, "scan error")?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardReply {
    pub api: u32,
    pub backend: String,
    pub result: BoardResult,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl BoardReply {
    pub fn new(backend: impl Into<String>, result: BoardResult) -> Self {
        Self {
            api: BOARD_API,
            backend: backend.into(),
            result,
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
    Plans(Vec<PlanRecord>),
    Plan(PlanView),
    Revision(RevisionRecord),
    Diff(RevisionDiff),
    Change(BoardChange),
    Review(ReviewEvidence),
    Feedback(Vec<FeedbackRecord>),
    Repositories(Vec<RepoScanTarget>),
    Registered(RepoRegistration),
    CommitsLinked(CommitLinkResult),
    Queued { import_key: FeedbackImportKey },
    ScanRecorded,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "state", rename_all = "snake_case")]
pub enum EntryState {
    Proposal(ProposalState),
    Feedback(FeedbackState),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntryRecord {
    pub id: EntryId,
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
pub struct TaskRecord {
    pub id: TaskId,
    pub title: PlanTitle,
    pub column: TaskColumn,
    pub assignee: Option<BoardRecipient>,
    pub section: Option<String>,
    pub seq: EventSeq,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimEndReason {
    Released,
    TakenOver,
    Reassigned,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClaimRecord {
    pub task: TaskId,
    pub actor: BoardActor,
    pub entry: EntryId,
    pub scope: EntryText,
    pub claimed_at: i64,
    pub last_active: i64,
    pub ended_at: Option<i64>,
    pub end_reason: Option<ClaimEndReason>,
    pub stale: bool,
    pub model: Option<String>,
    pub effort: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProposalRecord {
    pub entry: EntryId,
    pub plan: PlanId,
    pub base_revision: u64,
    pub body: PlanText,
    pub state: ProposalState,
    pub decision_entry: Option<EntryId>,
    pub result_revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventRecord {
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
    pub events: Vec<EventRecord>,
    pub open: Vec<EntryRecord>,
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlanView {
    pub plan: PlanRecord,
    pub revision: RevisionRecord,
    pub tasks: Vec<TaskRecord>,
    pub claims: Vec<ClaimRecord>,
    pub entries: Vec<EntryRecord>,
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
    pub open_proposals: Vec<ProposalRecord>,
    pub open_questions: Vec<EntryRecord>,
    pub open_feedback: Vec<FeedbackRecord>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RepoRegistration {
    pub repo_key: RepoKey,
    pub origin_label: Option<String>,
    pub host: String,
    pub common_dir: PathBuf,
    pub plan_id: Option<PlanId>,
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
pub struct CommitFileStat {
    pub path: String,
    pub insertions: Option<u64>,
    pub deletions: Option<u64>,
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
    pub file_stats: Vec<CommitFileStat>,
    pub plans: Vec<CommitPlanLink>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CommitLinkResult {
    pub inserted: u64,
    pub unknown_plans: Vec<PlanId>,
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versioned_requests_round_trip_and_reject_mismatch() {
        let actor = BoardActor::new(
            "josh",
            "laptop",
            HarnessLabel::parse("codex").unwrap(),
            "c1",
        )
        .unwrap();
        let mut request = BoardRequest::new(
            actor,
            BoardOp::Inbox {
                after: Some(EventSeq::new(0)),
                limit: 20,
            },
        );
        let decoded: BoardRequest =
            serde_json::from_str(&serde_json::to_string(&request).unwrap()).unwrap();
        assert_eq!(decoded, request);
        decoded.validate().unwrap();
        request.api += 1;
        assert!(
            request
                .validate()
                .unwrap_err()
                .to_string()
                .starts_with("board_api_mismatch:")
        );
    }

    #[test]
    fn internal_entry_kinds_and_open_feedback_cannot_be_posted_or_closed() {
        let post = BoardOp::Post {
            target: BoardRef::Plan(PlanId::new(1).unwrap()),
            kind: EntryKind::Claim,
            body: EntryText::new("scope").unwrap(),
            to: None,
            supersedes: None,
        };
        assert!(
            post.validate()
                .unwrap_err()
                .to_string()
                .starts_with("invalid_kind:")
        );
        let close = BoardOp::FeedbackClose {
            entry: EntryId::new(1).unwrap(),
            state: FeedbackState::Open,
            note: None,
        };
        assert!(
            close
                .validate()
                .unwrap_err()
                .to_string()
                .starts_with("invalid_state:")
        );
    }

    #[test]
    fn public_address_records_are_revalidated_at_the_request_boundary() {
        let plan = PlanId::new(1).unwrap();
        let bad_revision = BoardOp::Review {
            base: PlanRevision { plan, revision: 0 },
            agent: None,
        };
        let bad_task = BoardOp::ClaimTask {
            task: TaskId {
                plan,
                ordinal: u64::MAX,
            },
            scope: EntryText::new("owned scope").unwrap(),
        };
        assert!(bad_revision.validate().is_err());
        assert!(bad_task.validate().is_err());
    }

    #[test]
    fn feedback_enforces_recent_call_and_metadata_bounds() {
        let call = RecentCall {
            verb: "search".into(),
            args: vec!["symbol".into()],
            exit_code: Some(2),
            error_prefix: Some("unavailable".into()),
            truncated: None,
            coverage: None,
        };
        let mut metadata = FeedbackMetadata {
            recent_calls: vec![call; 6],
            ..FeedbackMetadata::default()
        };
        assert!(metadata.validate().is_err());
        metadata.recent_calls.truncate(5);
        metadata.validate().unwrap();
        metadata.build_id = Some("x".repeat(257));
        assert!(metadata.validate().is_err());
        metadata.build_id = None;
        metadata.recent_calls[0].args = vec!["x".repeat(2048)];
        assert!(metadata.validate().is_err());
    }

    #[test]
    fn typed_errors_preserve_domain_prefixes_and_database_lock_classification() {
        let stale = BoardError::from(anyhow::anyhow!("stale_revision: expected P1@2"));
        assert_eq!(stale.code, BoardErrorCode::StaleRevision);
        assert_eq!(stale.to_string(), "stale_revision: expected P1@2");
        let contextual = BoardError::from(
            anyhow::anyhow!("invalid_actor: not steward").context("accept proposal"),
        );
        assert_eq!(contextual.code, BoardErrorCode::InvalidActor);
        let locked = BoardError::from(anyhow::anyhow!("database is locked"));
        assert_eq!(locked.code, BoardErrorCode::DatabaseLocked);
        let unavailable = BoardError::from(anyhow::anyhow!("unable to open database file"));
        assert_eq!(unavailable.code, BoardErrorCode::BoardUnavailable);
    }
}
