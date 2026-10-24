//! Authority evidence codes carried by Board v10 records.
use crate::board::board_ids::EntryId;
use anyhow::Result;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, str::FromStr};

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum AuthoritySource {
    Owner,
    Grant(EntryId),
    Reporter,
    Creator,
    Stale,
    Holder,
    Delegator,
}

impl AuthoritySource {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "owner" => Ok(Self::Owner),
            "reporter" => Ok(Self::Reporter),
            "creator" => Ok(Self::Creator),
            "stale" => Ok(Self::Stale),
            "holder" => Ok(Self::Holder),
            "delegator" => Ok(Self::Delegator),
            _ => value
                .strip_prefix("grant:")
                .ok_or_else(|| {
                    anyhow::anyhow!(coded!(InvalidAuthority, "unknown source '{value}'"))
                })?
                .parse::<EntryId>()
                .map(Self::Grant),
        }
    }

    pub fn as_str(&self) -> String {
        self.to_string()
    }

    pub fn basis(&self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Grant(_) => "grant",
            Self::Reporter => "reporter",
            Self::Creator => "creator",
            Self::Stale => "stale",
            Self::Holder => "holder",
            Self::Delegator => "delegator",
        }
    }

    pub fn grant_entry(&self) -> Option<EntryId> {
        match self {
            Self::Grant(entry) => Some(*entry),
            _ => None,
        }
    }
}

impl fmt::Display for AuthoritySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Owner => f.write_str("owner"),
            Self::Grant(entry) => write!(f, "grant:{entry}"),
            Self::Reporter => f.write_str("reporter"),
            Self::Creator => f.write_str("creator"),
            Self::Stale => f.write_str("stale"),
            Self::Holder => f.write_str("holder"),
            Self::Delegator => f.write_str("delegator"),
        }
    }
}

impl FromStr for AuthoritySource {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}

impl Serialize for AuthoritySource {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for AuthoritySource {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_grant_source_round_trips_as_a_single_wire_code() {
        let source = AuthoritySource::Grant("E42".parse().unwrap());
        let encoded = serde_json::to_string(&source).unwrap();
        assert_eq!(encoded, r#""grant:E42""#);
        assert_eq!(
            serde_json::from_str::<AuthoritySource>(&encoded).unwrap(),
            source
        );
        assert_eq!(source.basis(), "grant");
        assert_eq!(source.grant_entry().unwrap().to_string(), "E42");
    }
}
