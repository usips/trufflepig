//! Closed board vocabulary and byte-bounded text accepted at the boundary.
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

pub const ENTRY_TEXT_LIMIT: usize = 4096;
pub const PLAN_TEXT_LIMIT: usize = 32768;
pub const PLAN_TITLE_LIMIT: usize = 256;

/// Stable outbox identity; canonical spelling is enforced at the boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct FeedbackImportKey(uuid::Uuid);
impl FeedbackImportKey {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
    pub fn parse(value: &str) -> Result<Self> {
        let id = uuid::Uuid::parse_str(value).map_err(|_| {
            anyhow::anyhow!("invalid_options: feedback import key must be a canonical UUID")
        })?;
        if id.to_string() != value {
            bail!("invalid_options: feedback import key must be a canonical UUID");
        }
        Ok(Self(id))
    }
}
impl Default for FeedbackImportKey {
    fn default() -> Self {
        Self::new()
    }
}
impl fmt::Display for FeedbackImportKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl TryFrom<String> for FeedbackImportKey {
    type Error = anyhow::Error;
    fn try_from(value: String) -> Result<Self> {
        Self::parse(&value)
    }
}
impl From<FeedbackImportKey> for String {
    fn from(value: FeedbackImportKey) -> Self {
        value.to_string()
    }
}

macro_rules! vocabulary {
    ($name:ident, $error:literal, {$($variant:ident => $label:literal),+ $(,)?}) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
        pub enum $name { $(#[serde(rename = $label)] $variant),+ }
        impl $name {
            pub fn parse(value: &str) -> Result<Self> {
                match value { $($label => Ok(Self::$variant),)+ _ => bail!("{}: unknown {} '{}'", $error, stringify!($name), value) }
            }
            pub fn as_str(self) -> &'static str { match self { $(Self::$variant => $label),+ } }
        }
        impl fmt::Display for $name { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) } }
        impl FromStr for $name { type Err = anyhow::Error; fn from_str(value: &str) -> Result<Self> { Self::parse(value) } }
    };
}

vocabulary!(EntryKind, "invalid_kind", {
    Note => "note", Progress => "progress", Review => "review", Question => "question",
    Answer => "answer", Decision => "decision", Divergence => "divergence", Claim => "claim",
    Commit => "commit", Feedback => "feedback", Task => "task", Hello => "hello",
    Create => "create", Proposal => "proposal", Accept => "accept", Reject => "reject", Direct => "direct",
});
impl EntryKind {
    pub fn is_post_kind(self) -> bool {
        matches!(
            self,
            Self::Note
                | Self::Progress
                | Self::Review
                | Self::Question
                | Self::Answer
                | Self::Decision
                | Self::Divergence
        )
    }
}

vocabulary!(TaskColumn, "invalid_state", {
    Todo => "todo", Doing => "doing", Review => "review", Done => "done", Blocked => "blocked",
});
vocabulary!(ProposalState, "invalid_state", {
    Open => "open", Accepted => "accepted", Rejected => "rejected", Superseded => "superseded",
});
vocabulary!(FeedbackKind, "invalid_kind", { Blocked => "blocked", Confused => "confused", Wrong => "wrong", Missing => "missing" });
vocabulary!(FeedbackState, "invalid_state", { Open => "open", Triaged => "triaged", Fixed => "fixed", Wontfix => "wontfix", Duplicate => "duplicate" });
impl FeedbackState {
    pub fn is_closed(self) -> bool {
        matches!(self, Self::Fixed | Self::Wontfix | Self::Duplicate)
    }
}

macro_rules! bounded_text {
    ($name:ident, $limit:ident, $blank:literal) => {
        #[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);
        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self> {
                let value = value.into();
                if value.len() > $limit {
                    bail!(
                        "invalid_body: {} exceeds {} bytes (received {})",
                        stringify!($name),
                        $limit,
                        value.len()
                    );
                }
                if value.contains('\0') {
                    bail!("invalid_body: text contains a NUL byte");
                }
                if !$blank && value.trim().is_empty() {
                    bail!("invalid_body: {} must not be blank", stringify!($name));
                }
                Ok(Self(value))
            }
            pub fn parse(value: &str) -> Result<Self> {
                Self::new(value)
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
            pub fn into_string(self) -> String {
                self.0
            }
        }
        impl TryFrom<String> for $name {
            type Error = anyhow::Error;
            fn try_from(value: String) -> Result<Self> {
                Self::new(value)
            }
        }
        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

bounded_text!(EntryText, ENTRY_TEXT_LIMIT, false);
bounded_text!(PlanText, PLAN_TEXT_LIMIT, true);
bounded_text!(PlanTitle, PLAN_TITLE_LIMIT, false);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_import_key_requires_canonical_uuid_at_deserialization() {
        let key = FeedbackImportKey::new();
        let encoded = serde_json::to_string(&key).unwrap();
        assert_eq!(
            serde_json::from_str::<FeedbackImportKey>(&encoded).unwrap(),
            key
        );
        for invalid in [
            "bad".to_owned(),
            "AAAAAAAA-AAAA-4AAA-AAAA-AAAAAAAAAAAA".to_owned(),
            key.to_string().replace('-', ""),
        ] {
            assert!(FeedbackImportKey::parse(&invalid).is_err());
            assert!(
                serde_json::from_str::<FeedbackImportKey>(
                    &serde_json::to_string(&invalid).unwrap()
                )
                .is_err()
            );
        }
    }

    #[test]
    fn entry_text_cap_counts_utf8_bytes_and_is_enforced_by_serde() {
        assert!(EntryText::new("x".repeat(4096)).is_ok());
        assert!(EntryText::new("x".repeat(4097)).is_err());
        assert!(EntryText::new("é".repeat(2048)).is_ok());
        assert!(EntryText::new("é".repeat(2049)).is_err());
        let encoded = serde_json::to_string(&"x".repeat(4097)).unwrap();
        assert!(serde_json::from_str::<EntryText>(&encoded).is_err());
        assert!(EntryText::new("\n  ").is_err());
        assert!(EntryText::new("a\0b").is_err());
    }

    #[test]
    fn plan_body_allows_empty_but_title_requires_content() {
        assert!(PlanText::new("").is_ok());
        assert!(PlanText::new("x".repeat(32768)).is_ok());
        assert!(PlanText::new("x".repeat(32769)).is_err());
        assert!(PlanTitle::new("").is_err());
        assert!(PlanTitle::new("x".repeat(256)).is_ok());
        assert!(PlanTitle::new("x".repeat(257)).is_err());
    }

    #[test]
    fn user_post_kinds_exclude_backend_event_kinds() {
        assert!(EntryKind::parse("progress").unwrap().is_post_kind());
        assert!(!EntryKind::Claim.is_post_kind());
        assert!(EntryKind::parse("unknown").is_err());
        assert!(TaskColumn::parse("active").is_err());
        assert!(!FeedbackState::Triaged.is_closed());
        assert!(FeedbackState::Fixed.is_closed());
    }
}
