//! Captured user/host identity and explicitly claimed harness/session labels.
use anyhow::{Result, bail};
use serde::{Deserialize, Deserializer, Serialize};
use std::{fmt, str::FromStr};

pub const ACTOR_COMPONENT_LIMIT: usize = 256;

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
            Self::Human => "human",
            Self::Unknown => "unknown",
        }
    }
}

pub(crate) fn validate_actor_component(value: &str, component: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > ACTOR_COMPONENT_LIMIT
        || value.chars().any(|character| {
            character.is_whitespace() || character.is_control() || matches!(character, '/' | '@')
        })
    {
        bail!(
            "invalid_actor: {component} must contain 1..256 bytes without whitespace, controls, '/' or '@'"
        );
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct HarnessLabel(String);
impl HarnessLabel {
    pub fn parse(value: &str) -> Result<Self> {
        if let Some(email) = value.strip_prefix("git:") {
            if email.is_empty()
                || value.len() > ACTOR_COMPONENT_LIMIT
                || email.chars().any(|character| {
                    character.is_whitespace() || character.is_control() || character == '/'
                })
            {
                bail!("invalid_actor: invalid git co-author label");
            }
            return Ok(Self(value.to_owned()));
        }
        if value.is_empty()
            || value.len() > ACTOR_COMPONENT_LIMIT
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            })
        {
            bail!("invalid_actor: harness must be a lowercase identifier");
        }
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn is_human(&self) -> bool {
        self.as_str() == "human"
    }
    pub fn is_cli(&self) -> bool {
        self.as_str() == "cli"
    }
}
/// Vendor attribution follows the claimed model, then the harness fallback.
pub fn claim_vendor(harness: &HarnessLabel, model: Option<&str>) -> AgentVendor {
    if harness.is_cli() || harness.is_human() {
        return AgentVendor::Human;
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

impl fmt::Display for HarnessLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl FromStr for HarnessLabel {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}
impl TryFrom<String> for HarnessLabel {
    type Error = anyhow::Error;
    fn try_from(value: String) -> Result<Self> {
        Self::parse(&value)
    }
}
impl From<HarnessLabel> for String {
    fn from(value: HarnessLabel) -> Self {
        value.0
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
        let harness = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("invalid_actor: expected user@host/harness/session"))?;
        let session = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("invalid_actor: expected user@host/harness/session"))?;
        if parts.next().is_some() {
            bail!("invalid_actor: expected exactly three identity components");
        }
        let (user, host) = user_host
            .split_once('@')
            .ok_or_else(|| anyhow::anyhow!("invalid_actor: expected user@host/harness/session"))?;
        Self::new(user, host, HarnessLabel::parse(harness)?, session)
    }
    pub fn identity(&self) -> String {
        self.to_string()
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
        if value.contains('/') {
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
    pub fn matches(&self, actor: &BoardActor) -> bool {
        self.as_str() == actor.user
            || self.as_str() == actor.harness.as_str()
            || self.as_str() == actor.identity()
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
