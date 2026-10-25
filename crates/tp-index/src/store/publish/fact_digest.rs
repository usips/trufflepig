//! Row digests and staged-to-live id translation for diff publication. Digests
//! exclude row ids and compare definition references in live id space.
use anyhow::{Context, Result};

/// Odd multiplier of the order-sensitive sequence digest.
const SEQUENCE_MULTIPLIER: u128 = 0x9e37_79b9_7f4a_7c15_f39c_c060_5ced_c835;

/// Encodes one row's columns unambiguously and hashes them.
#[derive(Default)]
pub(super) struct RowEncoder {
    buffer: Vec<u8>,
}

impl RowEncoder {
    pub(super) fn begin(&mut self) -> &mut Self {
        self.buffer.clear();
        self
    }

    pub(super) fn integer(&mut self, value: Option<i64>) -> &mut Self {
        match value {
            Some(value) => {
                self.buffer.push(1);
                self.buffer.extend_from_slice(&value.to_le_bytes());
            }
            None => self.buffer.push(0),
        }
        self
    }

    pub(super) fn text(&mut self, value: Option<&str>) -> &mut Self {
        match value {
            Some(value) => {
                self.buffer.push(2);
                self.buffer
                    .extend_from_slice(&(value.len() as u64).to_le_bytes());
                self.buffer.extend_from_slice(value.as_bytes());
            }
            None => self.buffer.push(0),
        }
        self
    }

    pub(super) fn hash(&self) -> u128 {
        let digest = blake3::hash(&self.buffer);
        u128::from_le_bytes(digest.as_bytes()[..16].try_into().expect("sixteen bytes"))
    }
}

/// An order-insensitive digest of a file's rows in one table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct RowSetDigest {
    sum: u128,
    count: u64,
}

impl RowSetDigest {
    pub(super) fn add(&mut self, row: u128) {
        self.sum = self.sum.wrapping_add(row);
        self.count += 1;
    }
}

/// An order-sensitive digest of a file's rows in id order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct RowSequenceDigest {
    accumulated: u128,
    count: u64,
}

impl RowSequenceDigest {
    pub(super) fn push(&mut self, row: u128) {
        self.accumulated = self
            .accumulated
            .wrapping_mul(SEQUENCE_MULTIPLIER)
            .wrapping_add(row);
        self.count += 1;
    }
}

/// Live ids of staged ids; staged ids are dense, so a vector indexes them.
pub(super) struct StagedIdMap {
    live: Vec<i64>,
}

impl StagedIdMap {
    pub(super) fn with_max_staged_id(max: i64) -> Result<Self> {
        Ok(Self {
            live: vec![0; usize::try_from(max.max(0))? + 1],
        })
    }

    pub(super) fn insert(&mut self, staged: i64, live: i64) -> Result<()> {
        let slot = usize::try_from(staged)
            .ok()
            .and_then(|staged| self.live.get_mut(staged))
            .with_context(|| format!("staged id {staged} out of range"))?;
        *slot = live;
        Ok(())
    }

    pub(super) fn get(&self, staged: i64) -> Result<i64> {
        usize::try_from(staged)
            .ok()
            .and_then(|staged| self.live.get(staged))
            .copied()
            .filter(|live| *live != 0)
            .with_context(|| format!("staged id {staged} has no live id"))
    }

    pub(super) fn get_optional(&self, staged: Option<i64>) -> Result<Option<i64>> {
        staged.map(|staged| self.get(staged)).transpose()
    }
}

/// Rewrites a JSON array of staged definition ids into live ids, keeping order.
pub(super) fn translate_candidates(
    candidates: &str,
    definitions: &StagedIdMap,
    output: &mut String,
) -> Result<()> {
    use std::fmt::Write;
    let inner = candidates
        .trim()
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .with_context(|| format!("candidate ids are not a JSON array: {candidates}"))?;
    output.clear();
    output.push('[');
    for (position, id) in inner
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .enumerate()
    {
        if position > 0 {
            output.push(',');
        }
        let id: i64 = id
            .parse()
            .with_context(|| format!("candidate id is not an integer: {candidates}"))?;
        let _ = write!(output, "{}", definitions.get(id)?);
    }
    output.push(']');
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_translate_in_order_and_reject_unknown_ids() {
        let mut map = StagedIdMap::with_max_staged_id(3).unwrap();
        map.insert(1, 40).unwrap();
        map.insert(3, 7).unwrap();
        let mut output = String::new();
        translate_candidates("[3,1]", &map, &mut output).unwrap();
        assert_eq!(output, "[7,40]");
        translate_candidates("[]", &map, &mut output).unwrap();
        assert_eq!(output, "[]");
        assert!(translate_candidates("[2]", &map, &mut output).is_err());
    }

    #[test]
    fn sequence_digest_is_order_sensitive_and_set_digest_is_not() {
        let (mut forward, mut backward) =
            (RowSequenceDigest::default(), RowSequenceDigest::default());
        let (mut forward_set, mut backward_set) =
            (RowSetDigest::default(), RowSetDigest::default());
        for row in [1_u128, 2] {
            forward.push(row);
            forward_set.add(row);
        }
        for row in [2_u128, 1] {
            backward.push(row);
            backward_set.add(row);
        }
        assert_ne!(forward, backward);
        assert_eq!(forward_set, backward_set);
    }
}
