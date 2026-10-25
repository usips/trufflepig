//! Captured user/host identity and explicitly claimed harness/session labels.
use anyhow::{Result, bail};
use serde::{Deserialize, Deserializer, Serialize};
use std::{fmt, str::FromStr};
use tp_core::request_context::{ActorOrigin, HarnessLabel, validate_actor_component};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentVendor {
    Claude,
    Codex,
    Kimi,
    Grok,
    Gemini,
    Qwen,
    Muse,
    System,
    Human,
    Unknown,
}

impl AgentVendor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Kimi => "kimi",
            Self::Grok => "grok",
            Self::Gemini => "gemini",
            Self::Qwen => "qwen",
            Self::Muse => "muse",
            Self::System => "system",
            Self::Human => "human",
            Self::Unknown => "unknown",
        }
    }
}

/// Vendor attribution follows the claimed model, then the harness fallback.
pub fn claim_vendor(harness: &HarnessLabel, model: Option<&str>) -> AgentVendor {
    if harness.is_human() {
        return AgentVendor::Human;
    }
    if harness.is_system() {
        return AgentVendor::System;
    }
    if harness.is_cli() {
        return AgentVendor::Unknown;
    }
    let model = model.unwrap_or_default().trim().to_ascii_lowercase();
    if model.starts_with("claude") {
        AgentVendor::Claude
    } else if model.starts_with("gpt") || model.starts_with("codex") || model.starts_with("chatgpt")
    {
        AgentVendor::Codex
    } else if model.starts_with("kimi") {
        AgentVendor::Kimi
    } else if model.starts_with("grok") {
        AgentVendor::Grok
    } else if model.starts_with("gemini") {
        AgentVendor::Gemini
    } else if model.starts_with("qwen") {
        AgentVendor::Qwen
    } else if model.starts_with("muse") || model.starts_with("llama") {
        AgentVendor::Muse
    } else {
        match harness.as_str() {
            "claude" => AgentVendor::Claude,
            "codex" => AgentVendor::Codex,
            "kimi" => AgentVendor::Kimi,
            "grok" => AgentVendor::Grok,
            "gemini" => AgentVendor::Gemini,
            "qwen" => AgentVendor::Qwen,
            "muse" => AgentVendor::Muse,
            _ => AgentVendor::Unknown,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize)]
pub struct BoardActor {
    pub user: String,
    pub host: String,
    pub harness: HarnessLabel,
    pub session: String,
}
impl BoardActor {
    pub fn new(
        user: impl Into<String>,
        host: impl Into<String>,
        harness: HarnessLabel,
        session: impl Into<String>,
    ) -> Result<Self> {
        let actor = Self {
            user: user.into(),
            host: host.into(),
            harness,
            session: session.into(),
        };
        actor.validate()?;
        Ok(actor)
    }
    /// Constructs the service identity used only for trusted maintenance requests.
    pub(crate) fn system(user: impl Into<String>, host: impl Into<String>) -> Result<Self> {
        Self::new(user, host, HarnessLabel::parse("system")?, "maintenance")
    }
    pub fn validate(&self) -> Result<()> {
        validate_actor_component(&self.user, "user")?;
        validate_actor_component(&self.host, "host")?;
        validate_actor_component(&self.session, "session")?;
        HarnessLabel::parse(self.harness.as_str())?;
        Ok(())
    }
    pub fn parse(value: &str) -> Result<Self> {
        let mut parts = value.split('/');
        let user_host = parts.next().unwrap_or_default();
        let harness = parts.next().ok_or_else(|| {
            anyhow::anyhow!(coded!(InvalidActor, "expected user@host/harness/session"))
        })?;
        let session = parts.next().ok_or_else(|| {
            anyhow::anyhow!(coded!(InvalidActor, "expected user@host/harness/session"))
        })?;
        if parts.next().is_some() {
            bail!(coded!(
                InvalidActor,
                "expected exactly three identity components"
            ));
        }
        let (user, host) = user_host.split_once('@').ok_or_else(|| {
            anyhow::anyhow!(coded!(InvalidActor, "expected user@host/harness/session"))
        })?;
        Self::new(user, host, HarnessLabel::parse(harness)?, session)
    }
    pub fn identity(&self) -> String {
        self.to_string()
    }
    /// The harness and session this actor asserts, without its user or host.
    pub fn origin(&self) -> ActorOrigin {
        ActorOrigin {
            harness: self.harness.clone(),
            session: self.session.clone(),
            peer_ip: None,
        }
    }
}
impl fmt::Display for BoardActor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}@{}/{}/{}",
            self.user, self.host, self.harness, self.session
        )
    }
}
impl FromStr for BoardActor {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}
impl<'de> Deserialize<'de> for BoardActor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            user: String,
            host: String,
            harness: HarnessLabel,
            session: String,
        }
        let fields = Fields::deserialize(deserializer)?;
        Self::new(fields.user, fields.host, fields.harness, fields.session)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BoardRecipient(String);
impl BoardRecipient {
    pub fn parse(value: &str) -> Result<Self> {
        if crate::board::board_ids::TaskId::parse(value).is_ok() {
            return Ok(Self(value.to_owned()));
        }
        if let Some((harness, session)) = value.split_once('/')
            && !session.contains('/')
        {
            HarnessLabel::parse(harness)?;
            validate_actor_component(session, "recipient session")?;
        } else if value.contains('/') {
            BoardActor::parse(value)?;
        } else if value.starts_with("git:") {
            HarnessLabel::parse(value)?;
        } else {
            validate_actor_component(value, "recipient")?;
        }
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn for_actor(actor: &BoardActor) -> Self {
        Self(actor.identity())
    }
    /// Expands a harness/session shorthand within the caller's user/host scope.
    pub fn normalize_for_caller(&self, caller: &BoardActor) -> Self {
        let Some((harness, session)) = self.as_str().split_once('/') else {
            return self.clone();
        };
        if session.contains('/') {
            return self.clone();
        }
        Self(format!(
            "{}@{}/{harness}/{session}",
            caller.user, caller.host
        ))
    }
    pub fn matches(&self, actor: &BoardActor) -> bool {
        let recipient = self.as_str();
        recipient == actor.user
            || recipient == actor.harness.as_str()
            || recipient == actor.identity()
            || recipient.split_once('/').is_some_and(|(harness, session)| {
                harness == actor.harness.as_str() && session == actor.session
            })
    }
}
impl fmt::Display for BoardRecipient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl FromStr for BoardRecipient {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}
impl TryFrom<String> for BoardRecipient {
    type Error = anyhow::Error;
    fn try_from(value: String) -> Result<Self> {
        Self::parse(&value)
    }
}
impl From<BoardRecipient> for String {
    fn from(value: BoardRecipient) -> Self {
        value.0
    }
}
