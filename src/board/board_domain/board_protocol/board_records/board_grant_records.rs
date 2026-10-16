//! Authority grants visible in plan views.
use crate::board::{
    board_actor::BoardRecipient,
    board_ids::{EntryId, PlanId},
    board_protocol::AuthorityRole,
};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantSource {
    Owner,
    Migration,
}

impl GrantSource {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "owner" => Ok(Self::Owner),
            "migration" => Ok(Self::Migration),
            _ => bail!("invalid_state: unknown grant source '{value}'"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Migration => "migration",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantRecord {
    pub entry: EntryId,
    pub plan: Option<PlanId>,
    pub owner_user: String,
    pub roles: Vec<AuthorityRole>,
    pub grantee: BoardRecipient,
    pub granted_at: i64,
    pub expires_at: Option<i64>,
    pub revoked_by: Option<EntryId>,
    pub source: GrantSource,
}
