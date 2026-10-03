//! Complete board reference parsing and free-text extraction.
use super::{EntryId, PlanId, PlanRevision, RevisionSpan, TaskId};
use crate::identity::GitOid;
use anyhow::Result;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, str::FromStr};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum BoardRef {
    Plan(PlanId),
    Revision(PlanRevision),
    Span(RevisionSpan),
    Task(TaskId),
    Entry(EntryId),
    Commit(GitOid),
}
impl BoardRef {
    pub fn parse(value: &str) -> Result<Self> {
        if matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return GitOid::parse(value).map(Self::Commit);
        }
        if value.starts_with('P') {
            if value.contains("..") {
                return RevisionSpan::parse(value).map(Self::Span);
            }
            if value.contains('@') {
                return PlanRevision::parse(value).map(Self::Revision);
            }
            if value.contains('.') {
                return TaskId::parse(value).map(Self::Task);
            }
            return PlanId::parse(value).map(Self::Plan);
        }
        if value.starts_with('E') {
            return EntryId::parse(value).map(Self::Entry);
        }
        GitOid::parse(value).map(Self::Commit).map_err(|_| {
            anyhow::anyhow!("invalid_reference: expected plan, revision, task, entry, or full oid")
        })
    }
    pub fn plan_id(self) -> Option<PlanId> {
        match self {
            Self::Plan(plan) => Some(plan),
            Self::Revision(value) => Some(value.plan),
            Self::Span(value) => Some(value.plan),
            Self::Task(value) => Some(value.plan),
            Self::Entry(_) | Self::Commit(_) => None,
        }
    }
    pub fn validate(self) -> Result<()> {
        match self {
            Self::Revision(value) => value.validate(),
            Self::Span(value) => value.validate(),
            Self::Task(value) => value.validate(),
            _ => Ok(()),
        }
    }
}
impl fmt::Display for BoardRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plan(value) => value.fmt(f),
            Self::Revision(value) => value.fmt(f),
            Self::Span(value) => value.fmt(f),
            Self::Task(value) => value.fmt(f),
            Self::Entry(value) => value.fmt(f),
            Self::Commit(value) => value.fmt(f),
        }
    }
}
impl FromStr for BoardRef {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}
string_serde!(BoardRef);

/// Extract distinct complete references; malformed IDs and abbreviated oids are ignored.
pub fn extract_refs(text: &str) -> Vec<BoardRef> {
    let mut references = Vec::new();
    for token in text.split(|character: char| {
        !(character.is_alphanumeric() || matches!(character, '_' | '@' | '.'))
    }) {
        let parsed =
            BoardRef::parse(token).or_else(|_| BoardRef::parse(token.trim_end_matches('.')));
        if let Ok(reference) = parsed {
            if !references.contains(&reference) {
                references.push(reference);
            }
        }
    }
    references
}
