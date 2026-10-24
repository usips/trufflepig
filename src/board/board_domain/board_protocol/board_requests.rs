//! Versioned backend requests and operation addresses.
use super::{
    ActorOrigin, AuthorityRole, BOARD_API, ClaimCursor, EntryCursor, FeedbackMetadata,
    LinkedCommit, OverviewCursor, ProgressInput, RepoRegistration, ReviewVerdict, TaskCeiling,
    TaskCursor, TaskOrder, WorkerAddress, WorkerHandle, validate_claim,
};
use crate::board::{
    board_actor::{BoardActor, BoardRecipient, HarnessLabel},
    board_ids::{BoardRef, EntryId, EventSeq, PlanId, PlanRevision, RepoKey, TaskId},
    board_vocabulary::{
        EntryKind, EntryText, FeedbackImportKey, FeedbackKind, FeedbackState, PlanText, PlanTitle,
        TaskColumn, WorkerEndReason, WorkerRole,
    },
};
use crate::identity::GitOid;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize, de};
use std::{fmt, path::PathBuf, str::FromStr};

mod board_op_addresses;
mod board_op_validation;
mod claim_fields;
mod read_scope;

pub use claim_fields::{ClaimDelegate, ClaimResume};
pub use read_scope::ReadScope;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardRequest {
    pub api: u32,
    pub actor: BoardActor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<ActorOrigin>,
    pub op: BoardOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claims: Option<AgentClaims>,
    #[serde(skip)]
    pub(crate) trusted_internal: bool,
    #[serde(skip)]
    pub(crate) trusted_head: Option<GitOid>,
    #[serde(skip)]
    observed_peer_ip: Option<std::net::IpAddr>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BoardRequestKey([u8; 32]);

impl BoardRequestKey {
    fn from_bytes(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }
}

impl fmt::Display for BoardRequestKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for BoardRequestKey {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        if value.len() != 64 {
            return Err("board receipt key must contain 64 hexadecimal characters".into());
        }
        let mut bytes = [0; 32];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            let high = hex_nibble(pair[0]).ok_or_else(|| {
                "board receipt key must contain only hexadecimal characters".to_owned()
            })?;
            let low = hex_nibble(pair[1]).ok_or_else(|| {
                "board receipt key must contain only hexadecimal characters".to_owned()
            })?;
            bytes[index] = (high << 4) | low;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for BoardRequestKey {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for BoardRequestKey {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(de::Error::custom)
    }
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
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
            origin: None,
            op,
            claims: None,
            trusted_internal: false,
            trusted_head: None,
            observed_peer_ip: None,
        }
    }

    pub(crate) fn dedupe_key(&self) -> Result<BoardRequestKey> {
        let bytes = if let BoardOp::Feedback {
            kind,
            summary,
            body,
            plan,
            ..
        } = &self.op
        {
            serde_json::to_vec(&(kind, summary, body, plan))?
        } else {
            serde_json::to_vec(self)?
        };
        Ok(BoardRequestKey::from_bytes(&bytes))
    }

    pub(crate) fn system(actor: BoardActor, op: BoardOp) -> Self {
        let origin = Some(ActorOrigin::from_actor(&actor));
        Self {
            api: BOARD_API,
            actor,
            origin,
            op,
            claims: None,
            trusted_internal: true,
            trusted_head: None,
            observed_peer_ip: None,
        }
    }

    pub(crate) fn observe_peer_ip(&mut self, peer_ip: std::net::IpAddr) {
        if self.origin.is_none() {
            self.origin = Some(ActorOrigin::from_actor(&self.actor));
        }
        let origin = self.origin.as_mut().expect("origin was initialized");
        origin.peer_ip = Some(peer_ip);
        self.observed_peer_ip = Some(peer_ip);
    }

    pub(crate) fn with_system_origin(mut self, session: impl Into<String>) -> Result<Self> {
        if !self.trusted_internal {
            bail!("invalid_actor: a system origin requires a trusted internal request");
        }
        self.origin = Some(ActorOrigin::system(session)?);
        Ok(self)
    }

    pub fn validate(&self) -> Result<()> {
        if self.api != BOARD_API {
            bail!(
                "board_api_mismatch: expected {BOARD_API}, received {}",
                self.api
            );
        }
        self.actor.validate()?;
        if self.actor.harness.is_system() != self.trusted_internal {
            bail!("invalid_actor: system actor requires a trusted internal request");
        }
        if self.actor.harness.as_str().starts_with("git:") {
            bail!("invalid_actor: git co-author labels cannot act as request harnesses");
        }
        if let Some(origin) = &self.origin {
            origin.validate_for_request(self.trusted_internal)?;
            if let Some(peer_ip) = origin.peer_ip {
                if self.observed_peer_ip != Some(peer_ip) {
                    bail!("invalid_origin: peer IP must be observed by the server");
                }
            }
        }
        if let Some(head) = self.trusted_head {
            match &self.op {
                BoardOp::Post {
                    progress: Some(progress),
                    ..
                } if progress.head == Some(head) => {}
                _ => bail!("invalid_progress: trusted head must match the progress OID"),
            }
        }
        if matches!(
            &self.op,
            BoardOp::WorkerEnd {
                state: WorkerEndReason::ClaimEnded,
                ..
            }
        ) && !self.trusted_internal
        {
            bail!("invalid_worker: claim_ended is reserved for trusted internal requests");
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worker: Option<WorkerAddress>,
    },
    Inbox {
        after: Option<EventSeq>,
        limit: usize,
        scope: ReadScope,
    },
    Receipt {
        key: BoardRequestKey,
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
    Projects,
    Overview {
        scope: ReadScope,
        after: Option<OverviewCursor>,
        #[serde(default)]
        active_only: bool,
        through: Option<EventSeq>,
        limit: usize,
    },
    Attention {
        scope: ReadScope,
        after: Option<EntryCursor>,
        through: Option<EventSeq>,
        limit: usize,
    },
    Feed {
        plan: Option<PlanId>,
        scope: ReadScope,
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
        column: Option<TaskColumn>,
        order: TaskOrder,
        before: Option<TaskCursor>,
        after: Option<TaskId>,
        ceiling: Option<TaskCeiling>,
        through: Option<EventSeq>,
        limit: usize,
    },
    DoneTasks {
        scope: ReadScope,
        before: Option<TaskCursor>,
        limit: usize,
    },
    Claims {
        plan: Option<PlanId>,
        own_stale: bool,
        scope: ReadScope,
        after: Option<ClaimCursor>,
        through: Option<EventSeq>,
        limit: usize,
    },
    New {
        title: PlanTitle,
        body: PlanText,
        repo_key: Option<RepoKey>,
    },
    Post {
        target: BoardRef,
        kind: EntryKind,
        body: EntryText,
        to: Option<BoardRecipient>,
        supersedes: Option<EntryId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress: Option<ProgressInput>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verdict: Option<ReviewVerdict>,
    },
    Grant {
        plan: Option<PlanId>,
        roles: Vec<AuthorityRole>,
        grantee: BoardRecipient,
        expires_in: Option<u64>,
        note: Option<EntryText>,
    },
    Revoke {
        entry: EntryId,
    },
    Bye,
    Pending {
        after: Option<EventSeq>,
        limit: usize,
    },
    WorkerAdd {
        task: TaskId,
        handle: WorkerHandle,
        role: WorkerRole,
        model: String,
        effort: Option<String>,
        scope: EntryText,
        files: Vec<String>,
    },
    WorkerEnd {
        worker: WorkerAddress,
        state: WorkerEndReason,
        note: Option<EntryText>,
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
    TaskTransfer {
        task: TaskId,
        destination: PlanId,
        note: EntryText,
    },
    PlanClose {
        plan: PlanId,
        note: EntryText,
    },
    PlanReopen {
        plan: PlanId,
        note: EntryText,
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
    /// Removes a stored task association without resolving Git or registering a repository.
    UnlinkCommit {
        oid: crate::identity::GitOid,
        task: TaskId,
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
