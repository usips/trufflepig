//! Closed board vocabulary and byte-bounded text accepted at the boundary.
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

pub const ENTRY_TEXT_LIMIT: usize = 32768;
pub const FEEDBACK_TEXT_LIMIT: usize = 4096;
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
            anyhow::anyhow!(coded!(
                InvalidOptions,
                "feedback import key must be a canonical UUID"
            ))
        })?;
        if id.to_string() != value {
            bail!(coded!(
                InvalidOptions,
                "feedback import key must be a canonical UUID"
            ));
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
    ($name:ident, $error:ident, {$($variant:ident => $label:literal),+ $(,)?}) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
        pub enum $name { $(#[serde(rename = $label)] $variant),+ }
        impl $name {
            pub fn parse(value: &str) -> Result<Self> {
                match value {
                    $($label => Ok(Self::$variant),)+
                    _ => bail!(coded!($error, "unknown {} '{}'", stringify!($name), value)),
                }
            }
            pub fn as_str(self) -> &'static str { match self { $(Self::$variant => $label),+ } }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
        impl FromStr for $name {
            type Err = anyhow::Error;
            fn from_str(value: &str) -> Result<Self> {
                Self::parse(value)
            }
        }
    };
}

vocabulary!(EntryKind, InvalidKind, {
    Note => "note", Progress => "progress", Review => "review", Question => "question",
    Answer => "answer", Decision => "decision", Divergence => "divergence", Claim => "claim",
    Commit => "commit", Unlinked => "unlinked", Feedback => "feedback", Task => "task",
    TaskTransfer => "task_transfer", Hello => "hello",
    Bye => "bye", Grant => "grant", Revoke => "revoke", WorkerAdd => "worker_add", WorkerEnd => "worker_end",
    Create => "create", Proposal => "proposal", Accept => "accept", Reject => "reject", Direct => "direct",
    PlanClose => "plan_close", PlanReopen => "plan_reopen",
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

vocabulary!(TaskColumn, InvalidState, {
    Todo => "todo", Doing => "doing", Review => "review", Done => "done", Blocked => "blocked",
    Transferred => "transferred",
});
vocabulary!(ProposalState, InvalidState, {
    Open => "open", Accepted => "accepted", Rejected => "rejected", Superseded => "superseded",
});
vocabulary!(FeedbackKind, InvalidKind, {
    Blocked => "blocked", Confused => "confused", Wrong => "wrong", Missing => "missing",
});
vocabulary!(FeedbackState, InvalidState, {
    Open => "open", Triaged => "triaged", Fixed => "fixed", Wontfix => "wontfix", Duplicate => "duplicate",
});

vocabulary!(AuthorityRole, InvalidRole, {
    Triage => "triage", Release => "release", Link => "link", Assign => "assign", Decide => "decide",
});

vocabulary!(WorkerRole, InvalidRole, {
    Code => "code", Review => "review", Test => "test", Research => "research",
});

vocabulary!(WorkerEndReason, InvalidState, {
    Done => "done", Failed => "failed", Stopped => "stopped", ClaimEnded => "claim_ended",
});
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
                    bail!(coded!(
                        InvalidBody,
                        "{} exceeds {} bytes (received {})",
                        stringify!($name),
                        $limit,
                        value.len()
                    ));
                }
                if value.contains('\0') {
                    bail!(coded!(InvalidBody, "text contains a NUL byte"));
                }
                if !$blank && value.trim().is_empty() {
                    bail!(coded!(
                        InvalidBody,
                        "{} must not be blank",
                        stringify!($name)
                    ));
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
    fn maximum_escaped_entry_fits_daemon_request_frame() {
        let text = "\u{1}".repeat(ENTRY_TEXT_LIMIT);
        let args = ["board", "post", "P1", "note", text.as_str()].map(str::to_owned);
        let context = tp_core::request_context::RequestContext::new(None, None);
        let encoded = tp_core::daemon_transport::request_encoded_size(&args, &context).unwrap();
        assert!(
            encoded > 64 * 1024,
            "fixture must exercise the raised bound"
        );
        assert!(encoded <= tp_core::daemon_transport::MAX_DAEMON_REQUEST_BYTES);
    }

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
    fn entry_text_cap_is_32k_utf8_bytes_and_is_enforced_by_serde() {
        assert!(EntryText::new("x".repeat(32768)).is_ok());
        assert!(EntryText::new("x".repeat(32769)).is_err());
        assert!(EntryText::new("é".repeat(16384)).is_ok());
        assert!(EntryText::new("é".repeat(16385)).is_err());
        let encoded = serde_json::to_string(&"x".repeat(32769)).unwrap();
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
        assert_eq!(
            EntryKind::parse("task_transfer").unwrap(),
            EntryKind::TaskTransfer
        );
        assert_eq!(
            EntryKind::parse("plan_close").unwrap(),
            EntryKind::PlanClose
        );
        assert_eq!(
            EntryKind::parse("plan_reopen").unwrap(),
            EntryKind::PlanReopen
        );
        assert!(!EntryKind::TaskTransfer.is_post_kind());
        assert!(!EntryKind::PlanClose.is_post_kind());
        assert!(!EntryKind::PlanReopen.is_post_kind());
        assert!(EntryKind::parse("unknown").is_err());
        assert!(TaskColumn::parse("active").is_err());
        assert_eq!(
            TaskColumn::parse("transferred").unwrap(),
            TaskColumn::Transferred
        );
        assert!(!FeedbackState::Triaged.is_closed());
        assert!(FeedbackState::Fixed.is_closed());
    }
}
