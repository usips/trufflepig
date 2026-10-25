//! Histogram changes over original-byte lines. All byte and line ranges are
//! end-exclusive; line indexes are zero based.

use crate::identity::ByteSpan;
use imara_diff::{Algorithm, Diff, InternedInput};
use serde::{Deserialize, Serialize};
use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceChange {
    pub before: ByteSpan,
    pub after: ByteSpan,
    pub before_lines: Range<usize>,
    pub after_lines: Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineCorrespondence {
    pub before: Range<usize>,
    pub after: Range<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SourceDiff {
    pub changes: Vec<SourceChange>,
    pub unchanged_lines: Vec<LineCorrespondence>,
}

impl SourceDiff {
    pub fn map_before_line(&self, line: usize) -> Option<usize> {
        let index = self
            .unchanged_lines
            .partition_point(|run| run.before.end <= line);
        self.unchanged_lines.get(index).and_then(|run| {
            run.before
                .contains(&line)
                .then(|| run.after.start + line - run.before.start)
        })
    }

    pub fn shares_change(&self, before_line: usize, after_line: usize) -> bool {
        let index = self
            .changes
            .partition_point(|change| change.before_lines.end <= before_line);
        self.changes.get(index).is_some_and(|change| {
            change.before_lines.contains(&before_line) && change.after_lines.contains(&after_line)
        })
    }
}

/// Compare bytes without normalizing encoding, whitespace, CRLF, or final newlines.
pub fn diff_sources(before: &[u8], after: &[u8]) -> SourceDiff {
    let input = InternedInput::new(before, after);
    diff_interned(&input, &line_offsets(before), &line_offsets(after))
}

pub fn diff_interned<T>(
    input: &InternedInput<T>,
    before_offsets: &[usize],
    after_offsets: &[usize],
) -> SourceDiff {
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_no_heuristic(&input);
    let hunk_count = diff.hunks().count();
    let mut result = SourceDiff {
        changes: Vec::with_capacity(hunk_count),
        unchanged_lines: Vec::with_capacity(hunk_count + 1),
    };
    let (mut before_end, mut after_end) = (0, 0);
    for hunk in diff.hunks() {
        let before_lines = hunk.before.start as usize..hunk.before.end as usize;
        let after_lines = hunk.after.start as usize..hunk.after.end as usize;
        if before_end < before_lines.start {
            result.unchanged_lines.push(LineCorrespondence {
                before: before_end..before_lines.start,
                after: after_end..after_lines.start,
            });
        }
        before_end = before_lines.end;
        after_end = after_lines.end;
        result.changes.push(SourceChange {
            before: ByteSpan {
                start: before_offsets[before_lines.start],
                end: before_offsets[before_lines.end],
            },
            after: ByteSpan {
                start: after_offsets[after_lines.start],
                end: after_offsets[after_lines.end],
            },
            before_lines,
            after_lines,
        });
    }
    if before_end < input.before.len() {
        result.unchanged_lines.push(LineCorrespondence {
            before: before_end..input.before.len(),
            after: after_end..input.after.len(),
        });
    }
    result
}

pub fn line_offsets(source: &[u8]) -> Vec<usize> {
    let count = source.iter().filter(|&&byte| byte == b'\n').count();
    let mut offsets = Vec::with_capacity(count + 2);
    offsets.push(0);
    for (index, &byte) in source.iter().enumerate() {
        if byte == b'\n' {
            offsets.push(index + 1);
        }
    }
    if offsets.last().copied() != Some(source.len()) {
        offsets.push(source.len());
    }
    offsets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_preserves_original_bytes_and_newlines() {
        let before = b"\xff\r\nkeep\nlast";
        let after = b"\xfe\r\nkeep\nlast\n";
        let diff = diff_sources(before, after);
        assert_eq!(diff.changes.len(), 2);
        assert_eq!(diff.changes[0].before, ByteSpan { start: 0, end: 3 });
        assert_eq!(diff.changes[0].after, ByteSpan { start: 0, end: 3 });
        assert_eq!(diff.changes[1].before, ByteSpan { start: 8, end: 12 });
        assert_eq!(diff.changes[1].after, ByteSpan { start: 8, end: 13 });
        assert_eq!(diff.map_before_line(1), Some(1));
        assert_eq!(diff.map_before_line(2), None);
    }

    #[test]
    fn histogram_handles_empty_insertions_deletions_and_crlf() {
        for (before, after) in [
            (&b""[..], &b"a\n"[..]),
            (&b"a\n"[..], &b""[..]),
            (&b"a\n"[..], &b"a\r\n"[..]),
            (&b"a b\n"[..], &b"a  b\n"[..]),
        ] {
            let diff = diff_sources(before, after);
            assert_eq!(diff.changes.len(), 1);
            assert_eq!(
                diff.changes[0].before,
                ByteSpan {
                    start: 0,
                    end: before.len()
                }
            );
            assert_eq!(
                diff.changes[0].after,
                ByteSpan {
                    start: 0,
                    end: after.len()
                }
            );
        }
        assert!(diff_sources(b"", b"").changes.is_empty());
    }
}
