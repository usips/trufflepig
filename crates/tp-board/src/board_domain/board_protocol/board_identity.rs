//! Client and worker identities carried by Board v11 evidence.
use crate::board_ids::TaskId;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

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
            bail!(coded!(
                InvalidWorker,
                "handle must be 1..32 lowercase ASCII letters, digits, '_' or '-'"
            ));
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
            .ok_or_else(|| anyhow::anyhow!(coded!(InvalidWorker, "expected P7.3/HANDLE")))?;
        if handle.contains('/') {
            bail!(coded!(InvalidWorker, "expected P7.3/HANDLE"));
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
}
