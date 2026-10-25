//! Harness labels and the client origin a request carries; board actors and
//! diagnostics share these validated identity components.
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::{fmt, net::IpAddr, str::FromStr};

pub const ACTOR_COMPONENT_LIMIT: usize = 256;

pub fn validate_actor_component(value: &str, component: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > ACTOR_COMPONENT_LIMIT
        || value.chars().any(|character| {
            character.is_whitespace() || character.is_control() || matches!(character, '/' | '@')
        })
    {
        bail!(coded!(
            InvalidActor,
            "{component} must contain 1..256 bytes without whitespace, controls, '/' or '@'"
        ));
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
                bail!(coded!(InvalidActor, "invalid git co-author label"));
            }
            return Ok(Self(value.to_owned()));
        }
        if value.is_empty()
            || value.len() > ACTOR_COMPONENT_LIMIT
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            })
        {
            bail!(coded!(
                InvalidActor,
                "harness must be a lowercase identifier"
            ));
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
    pub fn is_system(&self) -> bool {
        self.as_str() == "system"
    }
    /// Human, CLI, system, and git labels that no request may act as or delegate to.
    pub fn is_reserved(&self) -> bool {
        self.is_human() || self.is_cli() || self.is_system() || self.as_str().starts_with("git:")
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActorOrigin {
    pub harness: HarnessLabel,
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_ip: Option<IpAddr>,
}

impl ActorOrigin {
    pub fn system(session: impl Into<String>) -> Result<Self> {
        let origin = Self {
            harness: HarnessLabel::parse("system")?,
            session: session.into(),
            peer_ip: None,
        };
        origin.validate_for_request(true)?;
        Ok(origin)
    }

    pub fn validate(&self) -> Result<()> {
        self.validate_for_request(false)
    }

    pub fn validate_for_request(&self, trusted_internal: bool) -> Result<()> {
        HarnessLabel::parse(self.harness.as_str())?;
        if self.harness.is_system() && !trusted_internal {
            bail!(coded!(
                InvalidActor,
                "system cannot be supplied as a client origin"
            ));
        }
        validate_actor_component(&self.session, "session")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_origin_validation_rejects_system() {
        let origin = ActorOrigin {
            harness: HarnessLabel::parse("system").unwrap(),
            session: "maintenance".into(),
            peer_ip: None,
        };
        assert!(origin.validate().is_err());
        assert!(origin.validate_for_request(true).is_ok());
    }

    #[test]
    fn actor_origin_defaults_and_round_trips_peer_ip() {
        let legacy: ActorOrigin = serde_json::from_value(serde_json::json!({
            "harness": "codex",
            "session": "legacy-session"
        }))
        .unwrap();
        assert_eq!(legacy.peer_ip, None);
        assert!(
            serde_json::to_value(&legacy)
                .unwrap()
                .get("peer_ip")
                .is_none()
        );

        let origin = ActorOrigin {
            peer_ip: Some("192.168.0.42".parse().unwrap()),
            ..legacy
        };
        let encoded = serde_json::to_value(&origin).unwrap();
        assert_eq!(encoded["peer_ip"], "192.168.0.42");
        assert_eq!(
            serde_json::from_value::<ActorOrigin>(encoded)
                .unwrap()
                .peer_ip,
            origin.peer_ip
        );
    }
}
