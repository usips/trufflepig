//! Versioned backend requests and operation addresses.
use super::{
    BOARD_API, ClaimCursor, EntryCursor, FeedbackMetadata, LinkedCommit, RepoRegistration,
    TaskCeiling, validate_claim,
};
use crate::board::{
    board_actor::{BoardActor, BoardRecipient, HarnessLabel, validate_actor_component},
    board_ids::{BoardRef, EntryId, EventSeq, PlanId, PlanRevision, RepoKey, TaskId},
    board_vocabulary::{
        EntryKind, EntryText, FeedbackImportKey, FeedbackKind, FeedbackState, PlanText, PlanTitle,
        TaskColumn,
    },
};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

mod board_op_addresses;
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
        target: BoardRef,
    },
    Search {
        query: String,
        plan: Option<PlanId>,
        limit: usize,
    },
    Overview {
        repo_key: Option<RepoKey>,
        #[serde(default)]
        all: bool,
        after: Option<PlanId>,
        through: Option<EventSeq>,
        limit: usize,
    },
    Attention {
        repo_key: Option<RepoKey>,
        all: bool,
        after: Option<EntryCursor>,
        through: Option<EventSeq>,
        limit: usize,
    },
    Feed {
        plan: Option<PlanId>,
        after: Option<EventSeq>,
        through: Option<EventSeq>,
        limit: usize,
    },
    History {
        plan: PlanId,
        after: Option<EventSeq>,
        through: Option<EventSeq>,
        limit: usize,
    },
    Entries {
        plan: Option<PlanId>,
        kind: Option<EntryKind>,
        harness: Option<HarnessLabel>,
        user: Option<String>,
        host: Option<String>,
        task: Option<TaskId>,
        references: Option<EntryId>,
        after: Option<EntryCursor>,
        before: Option<EntryCursor>,
        through: Option<EventSeq>,
        limit: usize,
    },
    Tasks {
        plan: PlanId,
        after: Option<TaskId>,
        ceiling: Option<TaskCeiling>,
        through: Option<EventSeq>,
        limit: usize,
    },
    Claims {
        plan: Option<PlanId>,
        own_stale: bool,
        repo_key: Option<RepoKey>,
        all: bool,
        after: Option<ClaimCursor>,
        through: Option<EventSeq>,
        limit: usize,
    },
    New {
        title: PlanTitle,
        body: PlanText,
        steward: Option<HarnessLabel>,
        repo_key: Option<RepoKey>,
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
        resume: ClaimResume,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delegate: Option<ClaimDelegate>,
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
        after: Option<EntryCursor>,
        through: Option<EventSeq>,
        limit: usize,
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
    /// Manual repair link; the board host fills `resolution` from the plan's
    /// registered repositories before dispatch, so the wire carries no metadata.
    LinkCommit {
        oid: crate::identity::GitOid,
        task: TaskId,
        #[serde(skip)]
        resolution: Option<Box<LinkedCommit>>,
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

/// Delegated holder named by `--for HARNESS/SESSION`; the lease holder
/// resolves under the delegator's own user and host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimDelegate {
    pub harness: HarnessLabel,
    pub session: String,
}

impl ClaimDelegate {
    /// Parses a `--for` value; every malformed shape is `invalid_options`.
    pub fn parse(value: &str) -> Result<Self> {
        let malformed = || anyhow::anyhow!("invalid_options: --for must be HARNESS/SESSION");
        let (harness, session) = value.split_once('/').ok_or_else(malformed)?;
        if harness.is_empty() || session.is_empty() || session.contains('/') {
            bail!("invalid_options: --for must be HARNESS/SESSION");
        }
        if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
            bail!("invalid_options: --for must be HARNESS/SESSION");
        }
        let harness = HarnessLabel::parse(harness).map_err(|_| malformed())?;
        validate_actor_component(session, "session").map_err(|_| malformed())?;
        Ok(Self {
            harness,
            session: session.to_owned(),
        })
    }

    /// Resolves the holder identity under the delegator's user and host.
    pub fn holder(&self, delegator: &BoardActor) -> Result<BoardActor> {
        BoardActor::new(
            delegator.user.clone(),
            delegator.host.clone(),
            self.harness.clone(),
            self.session.clone(),
        )
    }
}

/// Resume intent carried by `claim_task`: none, an idle-grace resume, or a
/// deliberate takeover of the named unended claim entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaimResume {
    /// Ordinary claim without resume semantics.
    No,
    /// Bare `--resume`; refreshes the holder's own live lease, else waits for idle grace.
    Idle,
    /// `--resume=E#`; immediately replaces that exact unended claim entry.
    Entry(EntryId),
}

impl ClaimResume {
    /// Whether any resume semantics apply to the claim.
    pub fn is_resuming(self) -> bool {
        !matches!(self, Self::No)
    }
}

impl Serialize for ClaimResume {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::No => serializer.serialize_bool(false),
            Self::Idle => serializer.serialize_bool(true),
            Self::Entry(entry) => entry.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ClaimResume {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ClaimResumeVisitor;
        impl serde::de::Visitor<'_> for ClaimResumeVisitor {
            type Value = ClaimResume;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("false, true, or a claim entry reference")
            }

            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(if value {
                    ClaimResume::Idle
                } else {
                    ClaimResume::No
                })
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                EntryId::parse(value)
                    .map(ClaimResume::Entry)
                    .map_err(serde::de::Error::custom)
            }
        }
        deserializer.deserialize_any(ClaimResumeVisitor)
    }
}
