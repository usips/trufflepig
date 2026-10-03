//! Canonical board addresses and portable repository identities.
use anyhow::{Result, bail};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, str::FromStr};

pub const MAX_BOARD_NUMBER: u64 = i64::MAX as u64;

fn positive_number(value: &str) -> Result<u64> {
    if value.is_empty()
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        bail!("invalid_reference: expected a canonical positive number");
    }
    let number = value
        .parse::<u64>()
        .map_err(|_| anyhow::anyhow!("invalid_reference: number is too large"))?;
    check_positive(number)?;
    Ok(number)
}

fn check_positive(value: u64) -> Result<()> {
    if value == 0 || value > MAX_BOARD_NUMBER {
        bail!("invalid_reference: number must be 1..{}", MAX_BOARD_NUMBER);
    }
    Ok(())
}

macro_rules! string_serde {
    ($name:ty) => {
        impl Serialize for $name {
            fn serialize<S: Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.to_string())
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(
                deserializer: D,
            ) -> std::result::Result<Self, D::Error> {
                Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }
    };
}

macro_rules! numbered_id {
    ($name:ident, $prefix:literal) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
        pub struct $name(u64);
        impl $name {
            pub fn new(value: u64) -> Result<Self> {
                check_positive(value)?;
                Ok(Self(value))
            }
            pub fn get(self) -> u64 {
                self.0
            }
            pub fn parse(value: &str) -> Result<Self> {
                let suffix = value.strip_prefix($prefix).ok_or_else(|| {
                    anyhow::anyhow!(
                        "invalid_reference: expected {} followed by a number",
                        $prefix
                    )
                })?;
                Self::new(positive_number(suffix)?)
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}{}", $prefix, self.0)
            }
        }
        impl FromStr for $name {
            type Err = anyhow::Error;
            fn from_str(value: &str) -> Result<Self> {
                Self::parse(value)
            }
        }
        string_serde!($name);
    };
}

numbered_id!(PlanId, "P");
numbered_id!(EntryId, "E");

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct EventSeq(u64);

impl EventSeq {
    /// Construct a sequence already minted by SQLite; untrusted input uses parse.
    pub fn new(value: u64) -> Self {
        assert!(
            value <= MAX_BOARD_NUMBER,
            "event sequence exceeds SQLite integer range"
        );
        Self(value)
    }
    pub fn try_new(value: u64) -> Result<Self> {
        if value > MAX_BOARD_NUMBER {
            bail!("invalid_reference: event sequence exceeds SQLite integer range");
        }
        Ok(Self(value))
    }
    pub fn get(self) -> u64 {
        self.0
    }
    pub fn parse(value: &str) -> Result<Self> {
        if value == "0" {
            return Ok(Self(0));
        }
        Self::try_new(positive_number(value)?)
    }
}
impl fmt::Display for EventSeq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl FromStr for EventSeq {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}
impl Serialize for EventSeq {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_u64(self.0)
    }
}
impl<'de> Deserialize<'de> for EventSeq {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Self::try_new(u64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

mod board_plan_addresses;
mod board_refs;
mod board_repo_key;
#[cfg(test)]
mod tests;

pub use board_plan_addresses::{PlanRevision, RevisionSpan, TaskId};
pub use board_refs::{BoardRef, extract_refs};
pub use board_repo_key::RepoKey;
