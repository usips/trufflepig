//! Portable repository identities derived from canonical root commits.
use crate::identity::GitOid;
use anyhow::{Result, bail};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, str::FromStr};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct RepoKey(String);
impl RepoKey {
    pub fn from_roots(roots: impl IntoIterator<Item = GitOid>) -> Result<Self> {
        let mut roots: Vec<_> = roots.into_iter().collect();
        roots.sort_unstable();
        roots.dedup();
        if roots.is_empty() {
            bail!("invalid_reference: repository has no root commits");
        }
        let mut value = String::with_capacity(roots.iter().map(|oid| oid.as_str().len() + 1).sum());
        for (index, root) in roots.iter().enumerate() {
            if index != 0 {
                value.push(',');
            }
            value.push_str(root.as_str());
        }
        if value.len() > 32768 {
            bail!("invalid_reference: repository root identity exceeds 32768 bytes");
        }
        Ok(Self(value))
    }
    pub fn parse(value: &str) -> Result<Self> {
        if value.len() > 32768 {
            bail!("invalid_reference: repository root identity exceeds 32768 bytes");
        }
        let roots = value
            .split(',')
            .map(GitOid::parse)
            .collect::<Result<Vec<_>>>()
            .map_err(|error| anyhow::anyhow!("invalid_reference: repository roots: {error}"))?;
        let key = Self::from_roots(roots)?;
        if key.as_str() != value {
            bail!(
                "invalid_reference: repository roots must be sorted, unique, lowercase full oids"
            );
        }
        Ok(key)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for RepoKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl FromStr for RepoKey {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}
string_serde!(RepoKey);
