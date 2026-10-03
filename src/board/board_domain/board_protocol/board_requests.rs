//! Versioned backend requests and operation addresses.
use super::{BOARD_API, FeedbackMetadata, LinkedCommit, RepoRegistration, validate_claim};
use crate::board::{
    board_actor::{BoardActor, BoardRecipient, HarnessLabel},
    board_ids::{BoardRef, EntryId, EventSeq, PlanId, PlanRevision, RepoKey, TaskId},
    board_vocabulary::{
        EntryKind, EntryText, FeedbackImportKey, FeedbackKind, FeedbackState, PlanText, PlanTitle,
        TaskColumn,
    },
};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

mod board_op_validation;

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
        if self.actor.harness.as_str().starts_with("git:") {
            bail!("invalid_actor: git co-author labels cannot act as request harnesses");
        }
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
        repo_key: Option<RepoKey>,
        all: bool,
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
        scope: Option<EntryText>,
        resume: bool,
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
        supersedes: Option<EntryId>,
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
    FeedbackTriage {
        entry: EntryId,
        note: Option<EntryText>,
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
    ForgetRepoPath {
        repo_key: RepoKey,
        host: String,
        common_dir: PathBuf,
    },
    RecordScan {
        repo_key: RepoKey,
        host: String,
        common_dir: PathBuf,
        error: Option<String>,
    },
}

impl BoardOp {
    pub fn is_read_only(&self) -> bool {
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
}
