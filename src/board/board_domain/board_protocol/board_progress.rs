//! Typed progress input and field-level evidence facts.
use super::WorkerAddress;
use crate::{board::board_ids::EntryId, identity::GitOid};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

pub const PROGRESS_GATE_NAME_LIMIT: usize = 64;
pub const PROGRESS_ETA_LIMIT_SECS: u64 = 31_536_000;
pub const PROGRESS_GATE_LIMIT: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProgressStep {
    pub current: u64,
    pub total: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressHead {
    pub oid: GitOid,
    pub verified: bool,
}

impl ProgressStep {
    pub fn validate(self) -> Result<()> {
        if self.total == 0 || self.current > self.total {
            bail!(coded!(
                InvalidProgress,
                "step must satisfy 0 <= current <= total and total > 0"
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateStatus {
    Pass,
    Fail,
    Running,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressTests {
    pub passed: u64,
    pub failed: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressGateInput {
    pub name: String,
    pub status: GateStatus,
}

impl ProgressGateInput {
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty()
            || self.name.len() > PROGRESS_GATE_NAME_LIMIT
            || self.name.chars().any(char::is_control)
            || self.name.contains('=')
        {
            bail!(coded!(
                InvalidProgress,
                "gate name must be nonblank, at most 64 bytes, and contain no control characters or '='"
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<GitOid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<ProgressStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests: Option<ProgressTests>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<ProgressGateInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_secs: Option<u64>,
}

impl ProgressInput {
    pub fn validate(&self) -> Result<()> {
        if let Some(step) = self.step {
            step.validate()?;
        }
        if let Some(eta_secs) = self.eta_secs
            && (eta_secs == 0 || eta_secs > PROGRESS_ETA_LIMIT_SECS)
        {
            bail!(coded!(InvalidProgress, "ETA must be 1..=31536000 seconds"));
        }
        if self.gates.len() > PROGRESS_GATE_LIMIT {
            bail!(coded!(
                InvalidProgress,
                "at most 64 gate updates are allowed"
            ));
        }
        let mut names = std::collections::HashSet::with_capacity(self.gates.len());
        for gate in &self.gates {
            gate.validate()?;
            if !names.insert(&gate.name) {
                bail!(coded!(
                    InvalidProgress,
                    "a gate may be updated only once per entry"
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressFact<T> {
    pub value: T,
    pub entry: EntryId,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressGateFact {
    pub name: String,
    pub status: GateStatus,
    pub entry: EntryId,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressEvidence {
    pub claim: EntryId,
    #[serde(default)]
    pub worker: Option<WorkerAddress>,
    #[serde(default)]
    pub head: Option<ProgressFact<ProgressHead>>,
    #[serde(default)]
    pub step: Option<ProgressFact<ProgressStep>>,
    #[serde(default)]
    pub tests: Option<ProgressFact<ProgressTests>>,
    #[serde(default)]
    pub gates: Vec<ProgressGateFact>,
    #[serde(default)]
    pub eta_at: Option<ProgressFact<i64>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_rejects_invalid_step_eta_and_duplicate_gate_updates() {
        assert!(
            ProgressStep {
                current: 4,
                total: 3
            }
            .validate()
            .is_err()
        );
        assert!(
            ProgressStep {
                current: 0,
                total: 0
            }
            .validate()
            .is_err()
        );
        assert!(
            ProgressInput {
                eta_secs: Some(0),
                ..ProgressInput::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            ProgressInput {
                gates: vec![
                    ProgressGateInput {
                        name: "cargo".into(),
                        status: GateStatus::Running,
                    },
                    ProgressGateInput {
                        name: "cargo".into(),
                        status: GateStatus::Pass,
                    },
                ],
                ..ProgressInput::default()
            }
            .validate()
            .is_err()
        );
    }
}
