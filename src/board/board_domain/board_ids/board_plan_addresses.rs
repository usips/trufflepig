//! Canonical revision, revision-span, and task addresses within a plan.
use super::{PlanId, check_positive, positive_number};
use anyhow::{Result, bail};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, str::FromStr};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct PlanRevision {
    pub plan: PlanId,
    pub revision: u64,
}
impl PlanRevision {
    pub fn new(plan: PlanId, revision: u64) -> Result<Self> {
        check_positive(revision)?;
        Ok(Self { plan, revision })
    }
    pub fn validate(self) -> Result<()> {
        check_positive(self.revision)
    }
    pub fn parse(value: &str) -> Result<Self> {
        let (plan, revision) = value
            .split_once('@')
            .ok_or_else(|| anyhow::anyhow!("invalid_reference: expected P#@#"))?;
        Self::new(PlanId::parse(plan)?, positive_number(revision)?)
    }
}
impl fmt::Display for PlanRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.plan, self.revision)
    }
}
impl FromStr for PlanRevision {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}
string_serde!(PlanRevision);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct RevisionSpan {
    pub plan: PlanId,
    pub start: u64,
    pub end: Option<u64>,
}
impl RevisionSpan {
    pub fn new(plan: PlanId, start: u64, end: Option<u64>) -> Result<Self> {
        let span = Self { plan, start, end };
        span.validate()?;
        Ok(span)
    }
    pub fn validate(self) -> Result<()> {
        check_positive(self.start)?;
        if let Some(end) = self.end {
            check_positive(end)?;
            if end < self.start {
                bail!("invalid_reference: revision span is reversed");
            }
        }
        Ok(())
    }
    pub fn parse(value: &str) -> Result<Self> {
        let (base, end) = value
            .split_once("..")
            .ok_or_else(|| anyhow::anyhow!("invalid_reference: expected P#@#..[#]"))?;
        let base = PlanRevision::parse(base)?;
        Self::new(
            base.plan,
            base.revision,
            if end.is_empty() {
                None
            } else {
                Some(positive_number(end)?)
            },
        )
    }
}
impl fmt::Display for RevisionSpan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}..", self.plan, self.start)?;
        if let Some(end) = self.end {
            write!(f, "{end}")?;
        }
        Ok(())
    }
}
impl FromStr for RevisionSpan {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}
string_serde!(RevisionSpan);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct TaskId {
    pub plan: PlanId,
    pub ordinal: u64,
}
impl TaskId {
    pub fn new(plan: PlanId, ordinal: u64) -> Result<Self> {
        check_positive(ordinal)?;
        Ok(Self { plan, ordinal })
    }
    pub fn validate(self) -> Result<()> {
        check_positive(self.ordinal)
    }
    pub fn parse(value: &str) -> Result<Self> {
        let (plan, ordinal) = value
            .split_once('.')
            .ok_or_else(|| anyhow::anyhow!("invalid_reference: expected P#.#"))?;
        Self::new(PlanId::parse(plan)?, positive_number(ordinal)?)
    }
}
impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.plan, self.ordinal)
    }
}
impl FromStr for TaskId {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}
string_serde!(TaskId);
