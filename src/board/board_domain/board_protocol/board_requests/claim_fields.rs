//! Delegated-holder and resume-intent fields carried by claim requests.
use crate::board::board_actor::{BoardActor, HarnessLabel, validate_actor_component};
use crate::board::board_ids::EntryId;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

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
