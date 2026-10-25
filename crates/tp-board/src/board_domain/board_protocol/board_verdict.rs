//! Compact review outcomes attached to review entries and worker records.
use anyhow::{Result, bail};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, str::FromStr};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ReviewVerdict {
    Clean,
    Findings(u32),
    Blocking(u32),
}

impl ReviewVerdict {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "clean" => Ok(Self::Clean),
            _ => {
                if let Some(count) = value.strip_prefix("findings=") {
                    return count.parse::<u32>().map(Self::Findings).map_err(|_| {
                        anyhow::anyhow!(coded!(InvalidVerdict, "findings requires a u32 count"))
                    });
                }
                if let Some(count) = value.strip_prefix("blocking=") {
                    return count.parse::<u32>().map(Self::Blocking).map_err(|_| {
                        anyhow::anyhow!(coded!(InvalidVerdict, "blocking requires a u32 count"))
                    });
                }
                bail!(coded!(
                    InvalidVerdict,
                    "expected clean, findings=N, or blocking=N"
                ))
            }
        }
    }
}

impl fmt::Display for ReviewVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Clean => f.write_str("clean"),
            Self::Findings(count) => write!(f, "findings={count}"),
            Self::Blocking(count) => write!(f, "blocking={count}"),
        }
    }
}

impl FromStr for ReviewVerdict {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}

impl Serialize for ReviewVerdict {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ReviewVerdict {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_verdicts_use_compact_scalar_wire_values() {
        for (text, verdict) in [
            ("clean", ReviewVerdict::Clean),
            ("findings=12", ReviewVerdict::Findings(12)),
            ("blocking=2", ReviewVerdict::Blocking(2)),
        ] {
            assert_eq!(ReviewVerdict::parse(text).unwrap(), verdict);
            assert_eq!(
                serde_json::to_string(&verdict).unwrap(),
                format!("\"{text}\"")
            );
        }
        for invalid in ["findings=-1", "blocking=4294967296", "clean=0"] {
            assert!(ReviewVerdict::parse(invalid).is_err());
        }
    }
}
