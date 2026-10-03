//! Canonical board addresses and portable repository identities.
use crate::identity::GitOid;
use anyhow::{Result, bail};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, str::FromStr};

pub const MAX_BOARD_NUMBER: u64 = i64::MAX as u64;

fn positive_number(value: &str) -> Result<u64> {
    if value.is_empty()
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        bail!("invalid_reference: expected a canonical positive number");
    }
    let number = value
        .parse::<u64>()
        .map_err(|_| anyhow::anyhow!("invalid_reference: number is too large"))?;
    check_positive(number)?;
    Ok(number)
}

fn check_positive(value: u64) -> Result<()> {
    if value == 0 || value > MAX_BOARD_NUMBER {
        bail!("invalid_reference: number must be 1..{}", MAX_BOARD_NUMBER);
    }
    Ok(())
}

macro_rules! string_serde {
    ($name:ty) => {
        impl Serialize for $name {
            fn serialize<S: Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.to_string())
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(
                deserializer: D,
            ) -> std::result::Result<Self, D::Error> {
                Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }
    };
}

macro_rules! numbered_id {
    ($name:ident, $prefix:literal) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
        pub struct $name(u64);
        impl $name {
            pub fn new(value: u64) -> Result<Self> {
                check_positive(value)?;
                Ok(Self(value))
            }
            pub fn get(self) -> u64 {
                self.0
            }
            pub fn parse(value: &str) -> Result<Self> {
                let suffix = value.strip_prefix($prefix).ok_or_else(|| {
                    anyhow::anyhow!(
                        "invalid_reference: expected {} followed by a number",
                        $prefix
                    )
                })?;
                Self::new(positive_number(suffix)?)
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}{}", $prefix, self.0)
            }
        }
        impl FromStr for $name {
            type Err = anyhow::Error;
            fn from_str(value: &str) -> Result<Self> {
                Self::parse(value)
            }
        }
        string_serde!($name);
    };
}

numbered_id!(PlanId, "P");
numbered_id!(EntryId, "E");

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct EventSeq(u64);

impl EventSeq {
    /// Construct a sequence already minted by SQLite; untrusted input uses parse.
    pub fn new(value: u64) -> Self {
        assert!(
            value <= MAX_BOARD_NUMBER,
            "event sequence exceeds SQLite integer range"
        );
        Self(value)
    }
    pub fn try_new(value: u64) -> Result<Self> {
        if value > MAX_BOARD_NUMBER {
            bail!("invalid_reference: event sequence exceeds SQLite integer range");
        }
        Ok(Self(value))
    }
    pub fn get(self) -> u64 {
        self.0
    }
    pub fn parse(value: &str) -> Result<Self> {
        if value == "0" {
            return Ok(Self(0));
        }
        Self::try_new(positive_number(value)?)
    }
}
impl fmt::Display for EventSeq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl FromStr for EventSeq {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}
impl Serialize for EventSeq {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_u64(self.0)
    }
}
impl<'de> Deserialize<'de> for EventSeq {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Self::try_new(u64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

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

#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct RepoKey(String);
impl RepoKey {
    pub fn from_roots(roots: impl IntoIterator<Item = GitOid>) -> Result<Self> {
        let mut roots: Vec<_> = roots.into_iter().collect();
        roots.sort_unstable();
        roots.dedup();
        if roots.is_empty() {
            bail!("invalid_reference: repository has no root commits");
        }
        let mut value = String::with_capacity(roots.iter().map(|oid| oid.as_str().len() + 1).sum());
        for (index, root) in roots.iter().enumerate() {
            if index != 0 {
                value.push(',');
            }
            value.push_str(root.as_str());
        }
        if value.len() > 32768 {
            bail!("invalid_reference: repository root identity exceeds 32768 bytes");
        }
        Ok(Self(value))
    }
    pub fn parse(value: &str) -> Result<Self> {
        if value.len() > 32768 {
            bail!("invalid_reference: repository root identity exceeds 32768 bytes");
        }
        let roots = value
            .split(',')
            .map(GitOid::parse)
            .collect::<Result<Vec<_>>>()
            .map_err(|error| anyhow::anyhow!("invalid_reference: repository roots: {error}"))?;
        let key = Self::from_roots(roots)?;
        if key.as_str() != value {
            bail!(
                "invalid_reference: repository roots must be sorted, unique, lowercase full oids"
            );
        }
        Ok(key)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for RepoKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl FromStr for RepoKey {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}
string_serde!(RepoKey);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_addresses_round_trip_and_reject_zero_and_padding() {
        for value in ["P7", "P7@12", "P7@10..", "P7@10..14", "P7.3", "E482"] {
            let reference = BoardRef::parse(value).unwrap();
            assert_eq!(reference.to_string(), value);
            assert_eq!(
                serde_json::from_str::<BoardRef>(&serde_json::to_string(&reference).unwrap())
                    .unwrap(),
                reference
            );
        }
        for value in [
            "P0",
            "P07",
            "P7@0",
            "P7@01",
            "P7.0",
            "E00",
            "P7@14..10",
            "P7@1..0",
            "P9223372036854775808",
        ] {
            assert!(BoardRef::parse(value).is_err(), "accepted {value}");
        }
        assert!(serde_json::from_str::<EventSeq>("9223372036854775808").is_err());
        assert_eq!(EventSeq::parse("0").unwrap().get(), 0);
    }

    #[test]
    fn extraction_ignores_short_hex_and_ids_inside_words() {
        let oid = "0123456789abcdef0123456789abcdef01234567";
        let refs = extract_refs(&format!(
            "(P7.3), P7@12, E482. P7@10..14 P7.3 deadbeef {oid} xP7 P07 _E4"
        ));
        assert_eq!(refs.len(), 5);
        assert!(refs.contains(&BoardRef::Commit(GitOid::parse(oid).unwrap())));
        assert!(!refs.contains(&BoardRef::Plan(PlanId::new(7).unwrap())));
        assert!(extract_refs("abcdef0123456789abcdef0123456789abcdef0123").is_empty());
        let uppercase_oid = "E".repeat(40);
        assert!(matches!(
            BoardRef::parse(&uppercase_oid).unwrap(),
            BoardRef::Commit(_)
        ));
    }

    #[test]
    fn repository_roots_are_sorted_deduplicated_and_validated() {
        let first = GitOid::parse(&"a".repeat(40)).unwrap();
        let second = GitOid::parse(&"b".repeat(40)).unwrap();
        let key = RepoKey::from_roots([second, first, first]).unwrap();
        assert_eq!(key.to_string(), format!("{first},{second}"));
        assert_eq!(RepoKey::parse(key.as_str()).unwrap(), key);
        assert!(RepoKey::parse(&format!("{second},{first}")).is_err());
        assert!(RepoKey::from_roots([]).is_err());
    }
}
