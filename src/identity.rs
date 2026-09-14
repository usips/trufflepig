//! Validated object identifiers, original-byte spans, and immutable result addresses.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, str::FromStr};

/// Full SHA-1 or SHA-256 Git object ID; abbreviated IDs are not identities.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct GitOid {
    hex: [u8; 64],
    len: u8,
}

impl GitOid {
    pub fn parse(value: &str) -> Result<Self> {
        if !matches!(value.len(), 40 | 64) || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("invalid_oid: expected full SHA-1 or SHA-256 object ID");
        }
        let mut hex = [0; 64];
        for (out, byte) in hex.iter_mut().zip(value.bytes()) {
            *out = byte.to_ascii_lowercase();
        }
        Ok(Self {
            hex,
            len: value.len() as u8,
        })
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.hex[..self.len as usize]).expect("validated ASCII hex")
    }
}

impl fmt::Display for GitOid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl FromStr for GitOid {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}
impl Serialize for GitOid {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
impl<'de> Deserialize<'de> for GitOid {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// BLAKE3 identity of the complete original source buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContentRevision([u8; 32]);
impl ContentRevision {
    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }
    pub fn parse(value: &str) -> Result<Self> {
        let hash =
            blake3::Hash::from_hex(value).context("invalid_revision: expected BLAKE3 digest")?;
        Ok(Self(*hash.as_bytes()))
    }
}
impl TryFrom<String> for ContentRevision {
    type Error = anyhow::Error;
    fn try_from(value: String) -> Result<Self> {
        Self::parse(&value)
    }
}
impl From<ContentRevision> for String {
    fn from(value: ContentRevision) -> Self {
        blake3::Hash::from_bytes(value.0).to_hex().to_string()
    }
}
impl fmt::Display for ContentRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", blake3::Hash::from_bytes(self.0).to_hex())
    }
}

/// Half-open offsets in original bytes; validate against the acquired buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ByteSpan {
    pub start: usize,
    pub end: usize,
}
impl ByteSpan {
    pub fn new(start: usize, end: usize) -> Result<Self> {
        if start > end {
            bail!("invalid_span: reversed byte range");
        }
        Ok(Self { start, end })
    }
    pub fn validate(self, length: usize) -> Result<Self> {
        if self.start > self.end || self.end > length {
            bail!("invalid_span: coordinates outside verified source");
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResultHandle {
    pub set: uuid::Uuid,
    pub ordinal: usize,
}
impl FromStr for ResultHandle {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        let (set, ordinal) = value
            .rsplit_once(':')
            .context("invalid_handle: expected SET:ORDINAL")?;
        let set = uuid::Uuid::parse_str(set).context("invalid_handle: invalid result-set ID")?;
        let ordinal = ordinal
            .parse::<usize>()
            .context("invalid_handle: invalid ordinal")?;
        if ordinal == 0 {
            bail!("invalid_handle: ordinal must be positive");
        }
        Ok(Self { set, ordinal })
    }
}
impl fmt::Display for ResultHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.set.simple(), self.ordinal)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResultCursor {
    pub set: uuid::Uuid,
    pub offset: usize,
}
impl FromStr for ResultCursor {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        let (set, offset) = value
            .rsplit_once('@')
            .context("invalid_cursor: expected SET@OFFSET")?;
        Ok(Self {
            set: uuid::Uuid::parse_str(set).context("invalid_cursor: invalid result-set ID")?,
            offset: offset.parse().context("invalid_cursor: invalid offset")?,
        })
    }
}
impl fmt::Display for ResultCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.set.simple(), self.offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validated_identifiers_and_addresses() {
        for size in [40, 64] {
            let oid = GitOid::parse(&"A".repeat(size)).unwrap();
            assert_eq!(oid.as_str(), "a".repeat(size));
            assert_eq!(
                serde_json::from_str::<GitOid>(&serde_json::to_string(&oid).unwrap()).unwrap(),
                oid
            );
        }
        assert!(GitOid::parse("deadbeef").is_err());
        assert!(GitOid::parse(&"z".repeat(40)).is_err());
        let id = uuid::Uuid::new_v4().simple().to_string();
        assert!(format!("{id}:0").parse::<ResultHandle>().is_err());
        assert_eq!(
            format!("{id}:1")
                .parse::<ResultHandle>()
                .unwrap()
                .to_string(),
            format!("{id}:1")
        );
        assert!(ByteSpan::new(2, 1).is_err());
        assert!(ByteSpan::new(0, 3).unwrap().validate(2).is_err());
    }
}
