//! Session comparisons retain line and occurrence identities without retaining source bodies.

use super::{
    DeclarationCorrespondence, SourceDiff,
    correspondence::{MatchFact, match_facts},
    diff_interned, line_offsets,
};
use crate::identity::{ByteSpan, ContentRevision};
use anyhow::{Result, ensure};
use imara_diff::{InternedInput, TokenSource};
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FingerprintLines {
    pub offsets: Vec<usize>,
    pub fingerprints: Vec<ContentRevision>,
}

impl FingerprintLines {
    fn validate(&self) -> Result<usize> {
        ensure!(
            self.offsets.len() == self.fingerprints.len() + 1
                && self.offsets.first() == Some(&0)
                && self.offsets.windows(2).all(|pair| pair[0] < pair[1]),
            "invalid fingerprint line coordinates"
        );
        Ok(*self.offsets.last().expect("validated nonempty offsets"))
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct FingerprintFact {
    pub key: ContentRevision,
    pub span: ByteSpan,
    pub fingerprint: ContentRevision,
}

/// Hash complete original-byte lines, retaining CRLF and terminal newline distinctions.
pub fn fingerprint_lines(source: &[u8]) -> FingerprintLines {
    let offsets = line_offsets(source);
    let mut fingerprints = Vec::with_capacity(offsets.len() - 1);
    fingerprints.extend(
        offsets
            .windows(2)
            .map(|span| ContentRevision::of(&source[span[0]..span[1]])),
    );
    FingerprintLines {
        offsets,
        fingerprints,
    }
}

struct FingerprintTokens<'a>(&'a [ContentRevision]);

impl<'a> TokenSource for FingerprintTokens<'a> {
    type Token = ContentRevision;
    type Tokenizer = std::iter::Copied<std::slice::Iter<'a, ContentRevision>>;
    fn tokenize(&self) -> Self::Tokenizer {
        self.0.iter().copied()
    }
    fn estimate_tokens(&self) -> u32 {
        self.0.len() as u32
    }
}

/// Apply the same histogram and hunk normalization used by original-byte source comparisons.
pub fn diff_fingerprints(
    before: &FingerprintLines,
    after: &FingerprintLines,
) -> Result<SourceDiff> {
    before.validate()?;
    after.validate()?;
    let input = InternedInput::new(
        FingerprintTokens(&before.fingerprints),
        FingerprintTokens(&after.fingerprints),
    );
    Ok(diff_interned(&input, &before.offsets, &after.offsets))
}

/// Use persisted line evidence when available; otherwise require unique exact fingerprints.
/// Positional matches preserve duplicates; ambiguity and incomplete extraction remain explicit.
pub fn correspond_fingerprints(
    before: &[FingerprintFact],
    after: &[FingerprintFact],
    before_complete: bool,
    after_complete: bool,
    before_lines: Option<&FingerprintLines>,
    after_lines: Option<&FingerprintLines>,
) -> Result<Vec<DeclarationCorrespondence>> {
    let diff = match (before_lines, after_lines) {
        (Some(before), Some(after)) => diff_fingerprints(before, after)?,
        _ => SourceDiff::default(),
    };
    let before_length = before_lines.map(FingerprintLines::validate).transpose()?;
    let after_length = after_lines.map(FingerprintLines::validate).transpose()?;
    let convert = |facts: &[FingerprintFact], length: Option<usize>| -> Result<()> {
        for fact in facts {
            fact.span.validate(length.unwrap_or(usize::MAX))?;
        }
        Ok(())
    };
    convert(before, before_length)?;
    convert(after, after_length)?;
    let before_facts: Vec<_> = before
        .iter()
        .map(|fact| MatchFact {
            key: fact.key,
            start: fact.span.start,
            content: (fact.span.start < fact.span.end).then_some(&fact.fingerprint),
        })
        .collect();
    let after_facts: Vec<_> = after
        .iter()
        .map(|fact| MatchFact {
            key: fact.key,
            start: fact.span.start,
            content: (fact.span.start < fact.span.end).then_some(&fact.fingerprint),
        })
        .collect();
    Ok(match_facts(
        &before_facts,
        &after_facts,
        before_lines.map_or(&[0], |lines| lines.offsets.as_slice()),
        after_lines.map_or(&[0], |lines| lines.offsets.as_slice()),
        before_complete && after_complete,
        &diff,
    ))
}
