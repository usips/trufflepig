//! Client and worker identities carried by Board v11 evidence.
use crate::board::{
    board_actor::{BoardActor, HarnessLabel, validate_actor_component},
    board_ids::TaskId,
};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::{fmt, net::IpAddr, str::FromStr};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActorOrigin {
    pub harness: HarnessLabel,
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_ip: Option<IpAddr>,
}

impl ActorOrigin {
    pub(crate) fn system(session: impl Into<String>) -> Result<Self> {
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

    pub(crate) fn validate_for_request(&self, trusted_internal: bool) -> Result<()> {
        HarnessLabel::parse(self.harness.as_str())?;
        if self.harness.is_system() && !trusted_internal {
            bail!("invalid_actor: system cannot be supplied as a client origin");
        }
        validate_actor_component(&self.session, "session")
    }

    pub fn from_actor(actor: &BoardActor) -> Self {
        Self {
            harness: actor.harness.clone(),
            session: actor.session.clone(),
            peer_ip: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct WorkerHandle(String);

impl WorkerHandle {
    pub fn parse(value: &str) -> Result<Self> {
        if value.is_empty()
            || value.len() > 32
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte)
            })
        {
            bail!(
                "invalid_worker: handle must be 1..32 lowercase ASCII letters, digits, '_' or '-'"
            );
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkerHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for WorkerHandle {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}

impl TryFrom<String> for WorkerHandle {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        Self::parse(&value)
    }
}

impl From<WorkerHandle> for String {
    fn from(value: WorkerHandle) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerAddress {
    pub task: TaskId,
    pub handle: WorkerHandle,
}

impl WorkerAddress {
    pub fn new(task: TaskId, handle: WorkerHandle) -> Result<Self> {
        task.validate()?;
        Ok(Self { task, handle })
    }

    pub fn parse(value: &str) -> Result<Self> {
        let (task, handle) = value
            .split_once('/')
            .ok_or_else(|| anyhow::anyhow!("invalid_worker: expected P7.3/HANDLE"))?;
        if handle.contains('/') {
            bail!("invalid_worker: expected P7.3/HANDLE");
        }
        Self::new(task.parse()?, WorkerHandle::parse(handle)?)
    }

    pub fn validate(&self) -> Result<()> {
        self.task.validate()?;
        WorkerHandle::parse(self.handle.as_str())?;
        Ok(())
    }
}

impl fmt::Display for WorkerAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.task, self.handle)
    }
}

impl FromStr for WorkerAddress {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_addresses_require_canonical_task_scoped_lowercase_handles() {
        let address = WorkerAddress::parse("P7.3/parser_2").unwrap();
        assert_eq!(address.to_string(), "P7.3/parser_2");
        assert!(WorkerAddress::parse("P7.3/Parser").is_err());
        assert!(WorkerAddress::parse("P7.3/").is_err());
        assert!(WorkerAddress::parse("P7.3/worker/child").is_err());
        assert!(WorkerHandle::parse(&"a".repeat(33)).is_err());
    }

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
